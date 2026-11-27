//! System-wide memory cleanup via native Windows memory list and file cache APIs.
//!
//! Requires `SeProfileSingleProcessPrivilege` (for `SystemMemoryListInformation`)
//! and `SeIncreaseQuotaPrivilege` (for `SystemFileCacheInformationEx`), so
//! standard-user Argus routes this through the privileged service.

use std::mem::size_of;

use serde::{Deserialize, Serialize};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON};
use windows::Win32::UI::Shell::{
    QUNS_BUSY, QUNS_NOT_PRESENT, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
    SHQueryUserNotificationState,
};

use crate::actions::ActionError;
use crate::nt::{
    MEMORY_EMPTY_WORKING_SETS, MEMORY_FLUSH_MODIFIED_LIST, MEMORY_PURGE_STANDBY_LIST,
    NtSetSystemInformation, STATUS_PRIVILEGE_NOT_HELD, SYSTEM_FILE_CACHE_INFORMATION_EX,
    SYSTEM_MEMORY_LIST_INFORMATION, SystemFileCacheInformationEx,
};

/// Trim every process working set (`MemoryEmptyWorkingSets`).
pub const MASK_WORKING_SETS: u32 = 1 << 0;
/// Purge the standby page list (`MemoryPurgeStandbyList`).
pub const MASK_STANDBY_LIST: u32 = 1 << 1;
/// Flush the modified page list to disk (`MemoryFlushModifiedList`).
pub const MASK_MODIFIED_LIST: u32 = 1 << 2;
/// Trim the system file cache working set (`SystemFileCacheInformationEx`).
pub const MASK_SYSTEM_FILE_CACHE: u32 = 1 << 3;
/// Every valid cleanup bit.
pub const MASK_ALL: u32 =
    MASK_WORKING_SETS | MASK_STANDBY_LIST | MASK_MODIFIED_LIST | MASK_SYSTEM_FILE_CACHE;

/// Default scope for manual cleanup: working sets + standby list.
pub const DEFAULT_MANUAL_MASK: u32 = MASK_WORKING_SETS | MASK_STANDBY_LIST;

/// Mask stripped from automatic cleanup unless the user opts in to touching
/// standby/modified page lists automatically.
pub const AUTO_SKIP_FILE_CACHE_MASK: u32 = MASK_STANDBY_LIST | MASK_MODIFIED_LIST;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MemoryCleanupItem {
    WorkingSets,
    StandbyList,
    ModifiedList,
    SystemFileCache,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryCleanupItemResult {
    pub item: MemoryCleanupItem,
    pub ok: bool,
    pub privilege_not_held: bool,
    pub status: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryCleanupReport {
    pub mask: u32,
    pub available_before: u64,
    pub available_after: u64,
    pub freed_bytes: u64,
    pub privilege_not_held: bool,
    pub timed_out: bool,
    pub items: Vec<MemoryCleanupItemResult>,
}

/// Validates that `mask` selects at least one supported cleanup item and no
/// unknown bits.
pub fn validate_mask(mask: u32) -> Result<(), ActionError> {
    if mask == 0 || (mask & !MASK_ALL) != 0 {
        return Err(ActionError::Refused);
    }
    Ok(())
}

/// Filters `mask` for automatic cleanup: when `allow_file_cache` is false,
/// standby and modified page lists are skipped to avoid everyday stutter.
pub fn effective_auto_mask(mask: u32, allow_file_cache: bool) -> u32 {
    let valid = mask & MASK_ALL;
    if allow_file_cache {
        valid
    } else {
        valid & !AUTO_SKIP_FILE_CACHE_MASK
    }
}

/// Current available physical memory in bytes, or 0 if unavailable.
pub fn available_memory_bytes() -> u64 {
    let mut mem = MEMORYSTATUSEX {
        dwLength: size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    if unsafe { GlobalMemoryStatusEx(&mut mem) }.is_ok() {
        mem.ullAvailPhys
    } else {
        0
    }
}

/// Executes the selected memory cleanup operations sequentially.
///
/// If Windows returns `STATUS_PRIVILEGE_NOT_HELD`, subsequent items are skipped
/// and diagnostics are recorded so the UI can report insufficient privileges.
pub fn clean(mask: u32) -> Result<MemoryCleanupReport, ActionError> {
    validate_mask(mask)?;

    let available_before = available_memory_bytes();

    // Enable the privileges when the current token holds them (e.g., privileged
    // service or elevated Argus). Failure here is non-fatal; the kernel call
    // itself authoritatively reports STATUS_PRIVILEGE_NOT_HELD.
    let _ = crate::privilege::enable_profile_single_process_privilege();
    let _ = crate::privilege::enable_increase_quota_privilege();

    let mut report = MemoryCleanupReport {
        mask,
        available_before,
        available_after: available_before,
        freed_bytes: 0,
        privilege_not_held: false,
        timed_out: false,
        items: Vec::with_capacity(4),
    };

    // Order: trim process working sets and system file cache first, then flush
    // modified pages, and finally purge standby pages.
    let steps: [(u32, MemoryCleanupItem); 4] = [
        (MASK_WORKING_SETS, MemoryCleanupItem::WorkingSets),
        (MASK_SYSTEM_FILE_CACHE, MemoryCleanupItem::SystemFileCache),
        (MASK_MODIFIED_LIST, MemoryCleanupItem::ModifiedList),
        (MASK_STANDBY_LIST, MemoryCleanupItem::StandbyList),
    ];

    for (bit, item) in steps {
        if (mask & bit) == 0 {
            continue;
        }
        let status = match item {
            MemoryCleanupItem::WorkingSets => send_memory_list_command(MEMORY_EMPTY_WORKING_SETS),
            MemoryCleanupItem::ModifiedList => send_memory_list_command(MEMORY_FLUSH_MODIFIED_LIST),
            MemoryCleanupItem::StandbyList => send_memory_list_command(MEMORY_PURGE_STANDBY_LIST),
            MemoryCleanupItem::SystemFileCache => trim_system_file_cache(),
        };
        let ok = status >= 0;
        let priv_missing = status == STATUS_PRIVILEGE_NOT_HELD;
        if !ok {
            crate::diagnostics::warn(
                "memory cleanup",
                format!(
                    "item={item:?}; status=0x{:08x}; privilege_not_held={priv_missing}",
                    status as u32
                ),
            );
        }
        report.items.push(MemoryCleanupItemResult {
            item,
            ok,
            privilege_not_held: priv_missing,
            status,
        });
        if priv_missing {
            report.privilege_not_held = true;
            break;
        }
    }

    let available_after = available_memory_bytes();
    report.available_after = available_after;
    report.freed_bytes = available_after.saturating_sub(available_before);
    Ok(report)
}

fn send_memory_list_command(command: u32) -> i32 {
    unsafe {
        NtSetSystemInformation(
            SYSTEM_MEMORY_LIST_INFORMATION,
            (&command as *const u32).cast(),
            size_of::<u32>() as u32,
        )
        .0
    }
}

fn trim_system_file_cache() -> i32 {
    let info = SystemFileCacheInformationEx {
        minimum_working_set: usize::MAX,
        maximum_working_set: usize::MAX,
        ..Default::default()
    };
    unsafe {
        NtSetSystemInformation(
            SYSTEM_FILE_CACHE_INFORMATION_EX,
            (&info as *const SystemFileCacheInformationEx).cast(),
            size_of::<SystemFileCacheInformationEx>() as u32,
        )
        .0
    }
}

/// Returns true when automatic memory cleanup should be deferred to avoid
/// stutter: a fullscreen app, D3D game, presentation, or lock screen is active,
/// or a primary mouse button is currently held down (dragging).
pub fn should_defer_auto_cleanup() -> bool {
    // Active drag / mouse button held down.
    let left_down = (unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } as u16 & 0x8000) != 0;
    let right_down = (unsafe { GetAsyncKeyState(VK_RBUTTON.0 as i32) } as u16 & 0x8000) != 0;
    if left_down || right_down {
        return true;
    }

    // Fullscreen app, game, video, presentation mode, or lock screen.
    if let Ok(state) = unsafe { SHQueryUserNotificationState() } {
        return matches!(
            state,
            QUNS_BUSY | QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_PRESENTATION_MODE | QUNS_NOT_PRESENT
        );
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_cleanup_masks() {
        assert!(validate_mask(0).is_err());
        assert!(validate_mask(1 << 4).is_err());
        assert!(validate_mask(MASK_ALL | (1 << 31)).is_err());
        assert!(validate_mask(MASK_WORKING_SETS).is_ok());
        assert!(validate_mask(DEFAULT_MANUAL_MASK).is_ok());
        assert!(validate_mask(MASK_ALL).is_ok());
    }

    #[test]
    fn auto_mask_skips_standby_and_modified_by_default() {
        assert_eq!(
            effective_auto_mask(DEFAULT_MANUAL_MASK, false),
            MASK_WORKING_SETS
        );
        assert_eq!(
            effective_auto_mask(MASK_ALL, false),
            MASK_WORKING_SETS | MASK_SYSTEM_FILE_CACHE
        );
        assert_eq!(effective_auto_mask(MASK_ALL, true), MASK_ALL);
        assert_eq!(effective_auto_mask(MASK_STANDBY_LIST, false), 0);
    }
}

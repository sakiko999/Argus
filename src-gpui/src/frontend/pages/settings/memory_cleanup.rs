//! System memory cleanup: manual trigger, status feedback, and settings cards.
use super::*;
use crate::frontend::widgets::{ButtonExt, ToastKind, Tone};

fn cleanup_settings(settings: &Value) -> &Value {
    &settings["memoryCleanup"]
}

fn cleanup_bool(cfg: &Value, key: &str, default: bool) -> bool {
    cfg.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn cleanup_u64(cfg: &Value, key: &str, default: u64) -> u64 {
    cfg.get(key).and_then(Value::as_u64).unwrap_or(default)
}

fn item_label(item: &str) -> String {
    match item {
        "workingSets" => label("settings.memoryCleanup.scope.workingSets.title"),
        "standbyList" => label("settings.memoryCleanup.scope.standbyList.title"),
        "modifiedList" => label("settings.memoryCleanup.scope.modifiedList.title"),
        "systemFileCache" => label("settings.memoryCleanup.scope.systemFileCache.title"),
        other => other.to_owned(),
    }
}

impl Desktop {
    pub(in crate::frontend) fn memory_cleanup_busy(&self) -> bool {
        self.ui.busy.contains("cleanMemory")
    }

    /// Triggers manual memory cleanup: coalesces duplicate clicks, guides
    /// unprivileged users to Settings -> Permissions, otherwise asks for
    /// confirmation and runs `clean_memory`.
    pub(in crate::frontend) fn trigger_memory_cleanup(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.memory_cleanup_busy() {
            return;
        }
        if !self.privileged() {
            self.toast_with(
                ToastKind::Info,
                label("performance.memory.cleanupRequiresService"),
                8000,
                vec![(
                    label("settings.memoryCleanup.goToPrivilege"),
                    Rc::new(
                        |this: &mut Desktop, window: &mut Window, cx: &mut Context<Desktop>| {
                            this.locate_setting("privilege", "privilegedService", window, cx);
                        },
                    ),
                )],
            );
            return;
        }
        let cfg = cleanup_settings(&self.data["settings"]);
        let mut selected = Vec::new();
        if cleanup_bool(cfg, "workingSets", true) {
            selected.push(label("settings.memoryCleanup.scope.workingSets.title"));
        }
        if cleanup_bool(cfg, "standbyList", true) {
            selected.push(label("settings.memoryCleanup.scope.standbyList.title"));
        }
        if cleanup_bool(cfg, "modifiedList", false) {
            selected.push(label("settings.memoryCleanup.scope.modifiedList.title"));
        }
        if cleanup_bool(cfg, "systemFileCache", false) {
            selected.push(label("settings.memoryCleanup.scope.systemFileCache.title"));
        }
        if selected.is_empty() {
            self.toast_with(
                ToastKind::Info,
                label("performance.memory.cleanupNoScope"),
                6000,
                vec![(
                    label("settings.memoryCleanup.openSettings"),
                    Rc::new(
                        |this: &mut Desktop, window: &mut Window, cx: &mut Context<Desktop>| {
                            this.locate_setting("memoryCleanup", "memoryCleanupScope", window, cx);
                        },
                    ),
                )],
            );
            return;
        }
        let body = tr!(
            "performance.memory.cleanConfirmBody",
            items = selected.join(" / ")
        );
        self.ask(
            label("performance.memory.cleanConfirmTitle"),
            body,
            label("performance.memory.clean"),
            false,
            |this, window, cx| {
                if this.memory_cleanup_busy() {
                    return;
                }
                this.ui.busy.insert("cleanMemory".into());
                this.request("clean_memory", json!({}), None, window, cx);
            },
            window,
            cx,
        );
    }

    pub(super) fn memory_cleanup_completed(&mut self, value: &Value) {
        self.ui.busy.remove("cleanMemory");
        if boolean(value, "timedOut") {
            self.notify_toast(ToastKind::Error, label("performance.memory.cleanTimedOut"));
            return;
        }
        if boolean(value, "privilegeNotHeld") {
            self.toast_with(
                ToastKind::Error,
                label("performance.memory.cleanPrivilegeMissing"),
                10000,
                vec![(
                    label("settings.memoryCleanup.goToPrivilege"),
                    Rc::new(
                        |this: &mut Desktop, window: &mut Window, cx: &mut Context<Desktop>| {
                            this.locate_setting("privilege", "privilegedService", window, cx);
                        },
                    ),
                )],
            );
            return;
        }
        let before = theme::memory(number(value, "availableBefore"));
        let after = theme::memory(number(value, "availableAfter"));
        let freed = theme::memory(number(value, "freedBytes"));
        let failed: Vec<String> = values(&value["items"])
            .iter()
            .filter(|item| !boolean(item, "ok"))
            .map(|item| item_label(text(item, "item")))
            .collect();
        let message = if failed.is_empty() {
            tr!(
                "performance.memory.cleanDone",
                before = before,
                after = after,
                freed = freed
            )
        } else {
            tr!(
                "performance.memory.cleanPartial",
                before = before,
                after = after,
                freed = freed,
                failed = failed.join(", ")
            )
        };
        self.notify_toast_for(
            if failed.is_empty() {
                ToastKind::Success
            } else {
                ToastKind::Info
            },
            message,
            6000,
        );
    }

    pub(super) fn memory_cleanup_failed(&mut self, error: &Value, message: &str) {
        self.ui.busy.remove("cleanMemory");
        let kind = text(error, "kind");
        if matches!(kind, "accessDenied" | "serviceOutdated") {
            let text = if kind == "serviceOutdated" {
                label("performance.memory.cleanServiceOutdated")
            } else {
                label("performance.memory.cleanupRequiresService")
            };
            self.toast_with(
                ToastKind::Error,
                text,
                10000,
                vec![(
                    label("settings.memoryCleanup.goToPrivilege"),
                    Rc::new(
                        |this: &mut Desktop, window: &mut Window, cx: &mut Context<Desktop>| {
                            this.locate_setting("privilege", "privilegedService", window, cx);
                        },
                    ),
                )],
            );
        } else {
            self.notify_toast(
                ToastKind::Error,
                tr!("performance.memory.cleanFailed", error = message),
            );
        }
    }

    fn patch_memory_cleanup(&mut self, patch: Value, window: &mut Window, cx: &mut Context<Self>) {
        self.request(
            "set_memory_cleanup",
            json!({ "patch": patch }),
            Some("settings"),
            window,
            cx,
        );
    }

    pub(super) fn memory_cleanup_cards(
        &mut self,
        settings: &Value,
        cards: &mut Vec<AnyElement>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let p = self.palette;
        let cfg = cleanup_settings(settings);
        let interval = cleanup_u64(cfg, "intervalMinutes", 0);
        let threshold = cleanup_u64(cfg, "thresholdPercent", 0);
        let cooldown = cleanup_u64(cfg, "cooldownMinutes", 10);
        let working_sets = cleanup_bool(cfg, "workingSets", true);
        let standby_list = cleanup_bool(cfg, "standbyList", true);
        let modified_list = cleanup_bool(cfg, "modifiedList", false);
        let system_file_cache = cleanup_bool(cfg, "systemFileCache", false);
        let auto_clean_file_cache = cleanup_bool(cfg, "autoCleanFileCache", false);
        let defer_when_busy = cleanup_bool(cfg, "deferWhenBusy", true);

        // Status / privilege requirement card + manual clean shortcut
        let privileged = self.privileged();
        let busy = self.memory_cleanup_busy();
        let (state_text, tone, note_text) = if privileged {
            (
                label("settings.memoryCleanup.status.ready"),
                "ok",
                tr!("settings.memoryCleanup.cooldownNote", minutes = cooldown),
            )
        } else {
            (
                label("settings.memoryCleanup.status.needsPrivilege"),
                "warn",
                label("performance.memory.cleanupRequiresService"),
            )
        };
        let mut status_controls: Vec<AnyElement> = vec![];
        if !privileged {
            status_controls.push(
                self.btn(
                    "memory-cleanup-go-privilege",
                    label("settings.memoryCleanup.goToPrivilege"),
                    None,
                    Tone::Plain,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.locate_setting("privilege", "privilegedService", window, cx);
                }))
                .into_any_element(),
            );
        } else {
            status_controls.push(
                self.btn(
                    "memory-cleanup-run-now",
                    if busy {
                        label("performance.memory.cleaning")
                    } else {
                        label("performance.memory.clean")
                    },
                    None,
                    Tone::Plain,
                )
                .inert(busy)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.trigger_memory_cleanup(window, cx);
                }))
                .into_any_element(),
            );
        }
        cards.push(self.feature(
            "memoryCleanupStatus",
            "MemoryIcon",
            label("settings.memoryCleanup.title"),
            label("settings.memoryCleanup.desc"),
            state_text,
            tone,
            div().child(note_text).into_any_element(),
            None,
            status_controls,
            None,
        ));

        // 1. Auto cleanup by interval: Off / 30m / 1h / 3h
        let intervals: [(u64, &str); 4] = [
            (0, "settings.memoryCleanup.interval.off"),
            (30, "settings.memoryCleanup.interval.m30"),
            (60, "settings.memoryCleanup.interval.h1"),
            (180, "settings.memoryCleanup.interval.h3"),
        ];
        let interval_control = self.segments(
            "setting-memoryCleanupInterval",
            intervals
                .into_iter()
                .map(|(mins, key)| {
                    (
                        label(key),
                        None,
                        Action::Invoke(
                            "set_memory_cleanup".into(),
                            json!({"patch": {"intervalMinutes": mins}}),
                        ),
                        interval == mins,
                    )
                })
                .collect(),
            cx,
        );
        cards.push(self.setting_text(
            "memoryCleanupInterval",
            "TimerIcon",
            "settings.memoryCleanup.interval.title",
            "settings.memoryCleanup.interval.desc",
            vec![interval_control],
        ));

        // 2. Auto cleanup by memory usage threshold: Off / 85% / 90% / 95% + current live reading
        let sys = &self.snapshot["system"];
        let total = number(sys, "memTotal");
        let used = number(sys, "memUsed");
        let avail = number(sys, "memAvailable");
        let current_pct = if total > 0. {
            (used / total * 100.).clamp(0., 100.)
        } else {
            0.
        };
        let threshold_desc = if total > 0. {
            format!(
                "{}\n{}",
                label("settings.memoryCleanup.threshold.desc"),
                tr!(
                    "settings.memoryCleanup.threshold.current",
                    percent = crate::regional::number!("{:.0}%", current_pct),
                    available = theme::memory(avail)
                )
            )
        } else {
            label("settings.memoryCleanup.threshold.desc")
        };
        let thresholds: [(u64, String); 4] = [
            (0, label("settings.memoryCleanup.threshold.off")),
            (85, "85%".into()),
            (90, "90%".into()),
            (95, "95%".into()),
        ];
        let threshold_control = self.segments(
            "setting-memoryCleanupThreshold",
            thresholds
                .into_iter()
                .map(|(pct, text)| {
                    (
                        text,
                        None,
                        Action::Invoke(
                            "set_memory_cleanup".into(),
                            json!({"patch": {"thresholdPercent": pct}}),
                        ),
                        threshold == pct,
                    )
                })
                .collect(),
            cx,
        );
        cards.push(self.setting(
            "memoryCleanupThreshold",
            self.glyph("GaugeIcon"),
            label("settings.memoryCleanup.threshold.title"),
            Some(div().child(threshold_desc).into_any_element()),
            None,
            vec![threshold_control],
        ));

        // 3. Defer when busy (fullscreen app, game, dragging, lock screen)
        let defer_toggle = self.toggle(
            "setting-memoryCleanupDefer",
            label("settings.memoryCleanup.defer.title"),
            defer_when_busy,
            false,
            |this, on, window, cx| {
                this.patch_memory_cleanup(json!({"deferWhenBusy": on}), window, cx);
            },
            window,
            cx,
        );
        cards.push(self.setting_text(
            "memoryCleanupDefer",
            "DisplayIcon",
            "settings.memoryCleanup.defer.title",
            "settings.memoryCleanup.defer.desc",
            vec![defer_toggle],
        ));

        // 4. Cleanup scope (4 items + auto-cleanup file cache policy)
        let scope_row = |this: &Self,
                         id: &'static str,
                         title_key: &'static str,
                         desc_key: &'static str,
                         value: bool,
                         field: &'static str,
                         window: &mut Window,
                         cx: &mut Context<Self>| {
            let toggle = this.toggle(
                format!("setting-memoryCleanup-{id}"),
                label(title_key),
                value,
                false,
                move |this, on, window, cx| {
                    this.patch_memory_cleanup(json!({ field: on }), window, cx);
                },
                window,
                cx,
            );
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(12.))
                .py(px(8.))
                .border_t_1()
                .border_color(p.stroke)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().text_size(theme::font(13.)).child(label(title_key)))
                        .child(
                            div()
                                .mt(px(2.))
                                .text_size(theme::font(12.))
                                .text_color(p.muted)
                                .child(label(desc_key)),
                        ),
                )
                .child(toggle)
        };

        let scope_body = div()
            .flex()
            .flex_col()
            .child(scope_row(
                self,
                "workingSets",
                "settings.memoryCleanup.scope.workingSets.title",
                "settings.memoryCleanup.scope.workingSets.desc",
                working_sets,
                "workingSets",
                window,
                cx,
            ))
            .child(scope_row(
                self,
                "standbyList",
                "settings.memoryCleanup.scope.standbyList.title",
                "settings.memoryCleanup.scope.standbyList.desc",
                standby_list,
                "standbyList",
                window,
                cx,
            ))
            .child(scope_row(
                self,
                "modifiedList",
                "settings.memoryCleanup.scope.modifiedList.title",
                "settings.memoryCleanup.scope.modifiedList.desc",
                modified_list,
                "modifiedList",
                window,
                cx,
            ))
            .child(scope_row(
                self,
                "systemFileCache",
                "settings.memoryCleanup.scope.systemFileCache.title",
                "settings.memoryCleanup.scope.systemFileCache.desc",
                system_file_cache,
                "systemFileCache",
                window,
                cx,
            ))
            .child(scope_row(
                self,
                "autoCleanFileCache",
                "settings.memoryCleanup.scope.autoCleanFileCache.title",
                "settings.memoryCleanup.scope.autoCleanFileCache.desc",
                auto_clean_file_cache,
                "autoCleanFileCache",
                window,
                cx,
            ))
            .into_any_element();

        cards.push(self.setting_with_body(
            "memoryCleanupScope",
            "ListIcon",
            "settings.memoryCleanup.scope.title",
            "settings.memoryCleanup.scope.desc",
            scope_body,
        ));
    }
}

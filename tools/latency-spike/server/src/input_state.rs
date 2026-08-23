//! Portable bookkeeping for injected input that must be released on disconnect.

use std::time::Duration;

use crate::input_proto::MouseButton;

pub const INPUT_DESKTOP_SYNC_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, Default)]
pub struct DesktopSyncCadence {
    last_sync: Option<Duration>,
}

impl DesktopSyncCadence {
    pub fn due(&mut self, elapsed: Duration) -> bool {
        let due = self
            .last_sync
            .is_none_or(|last| elapsed.saturating_sub(last) >= INPUT_DESKTOP_SYNC_INTERVAL);
        if due {
            self.last_sync = Some(elapsed);
        }
        due
    }
}

pub fn deliver_with_desktop_sync<E>(
    sync_due: bool,
    mut sync: impl FnMut() -> Result<(), E>,
    mut send: impl FnMut() -> bool,
) -> Result<bool, E> {
    if sync_due {
        sync()?;
    }
    if send() {
        return Ok(true);
    }
    sync()?;
    Ok(send())
}

pub fn trusted_ssh_peer_image(image: &str, windows_root: &str) -> bool {
    let normalise = |value: &str| value.replace('/', "\\").to_ascii_lowercase();
    normalise(image)
        == format!(
            "{}\\system32\\openssh\\sshd.exe",
            normalise(windows_root).trim_end_matches('\\')
        )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputTransition {
    VirtualKey { vk: u16, down: bool },
    Scancode { scancode: u16, down: bool },
    MouseButton { button: MouseButton, down: bool },
}

#[derive(Debug, Default)]
pub struct HeldInputs {
    virtual_keys: Vec<u16>,
    scancodes: Vec<u16>,
    mouse_buttons: Vec<MouseButton>,
}

impl HeldInputs {
    pub fn apply(&mut self, transition: InputTransition) {
        match transition {
            InputTransition::VirtualKey { vk, down } => {
                update(&mut self.virtual_keys, vk, down);
            }
            InputTransition::Scancode { scancode, down } => {
                update(&mut self.scancodes, scancode, down);
            }
            InputTransition::MouseButton { button, down } => {
                update(&mut self.mouse_buttons, button, down);
            }
        }
    }

    pub fn release_plan(&mut self) -> Vec<InputTransition> {
        let mut releases = Vec::with_capacity(
            self.virtual_keys.len() + self.scancodes.len() + self.mouse_buttons.len(),
        );
        releases.extend(
            self.virtual_keys
                .drain(..)
                .map(|vk| InputTransition::VirtualKey { vk, down: false }),
        );
        releases.extend(
            self.scancodes
                .drain(..)
                .map(|scancode| InputTransition::Scancode {
                    scancode,
                    down: false,
                }),
        );
        releases.extend(
            self.mouse_buttons
                .drain(..)
                .map(|button| InputTransition::MouseButton {
                    button,
                    down: false,
                }),
        );
        releases
    }
}

fn update<T: PartialEq>(held: &mut Vec<T>, value: T, down: bool) {
    if down {
        if !held.contains(&value) {
            held.push(value);
        }
    } else if let Some(index) = held.iter().position(|held| *held == value) {
        held.remove(index);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        deliver_with_desktop_sync, trusted_ssh_peer_image, DesktopSyncCadence, HeldInputs,
        InputTransition, INPUT_DESKTOP_SYNC_INTERVAL,
    };
    use crate::input_proto::MouseButton;
    use std::cell::RefCell;
    use std::time::Duration;

    #[test]
    fn desktop_sync_is_immediate_then_bounded_to_four_checks_per_second() {
        let mut cadence = DesktopSyncCadence::default();
        assert!(cadence.due(Duration::ZERO));
        assert!(!cadence.due(INPUT_DESKTOP_SYNC_INTERVAL - Duration::from_millis(1)));
        assert!(cadence.due(INPUT_DESKTOP_SYNC_INTERVAL));
        assert!(!cadence.due(INPUT_DESKTOP_SYNC_INTERVAL));
    }

    #[test]
    fn rejected_input_synchronises_and_retries_exactly_once() {
        let calls = RefCell::new(Vec::new());
        let mut attempts = 0;
        let delivered = deliver_with_desktop_sync(
            false,
            || {
                calls.borrow_mut().push("sync");
                Ok::<_, ()>(())
            },
            || {
                calls.borrow_mut().push("send");
                attempts += 1;
                attempts == 2
            },
        )
        .unwrap();

        assert!(delivered);
        assert_eq!(*calls.borrow(), ["send", "sync", "send"]);
    }

    #[test]
    fn successful_input_inside_the_interval_stays_on_the_one_call_fast_path() {
        let calls = RefCell::new(Vec::new());
        let delivered = deliver_with_desktop_sync(
            false,
            || {
                calls.borrow_mut().push("sync");
                Ok::<_, ()>(())
            },
            || {
                calls.borrow_mut().push("send");
                true
            },
        )
        .unwrap();

        assert!(delivered);
        assert_eq!(*calls.borrow(), ["send"]);
    }

    #[test]
    fn only_the_protected_windows_openssh_image_is_a_trusted_input_peer() {
        assert!(trusted_ssh_peer_image(
            r"C:\Windows\System32\OpenSSH\sshd.exe",
            r"C:\Windows"
        ));
        assert!(trusted_ssh_peer_image(
            r"c:/windows/system32/openssh/SSHD.EXE",
            r"C:\Windows\\"
        ));
        assert!(!trusted_ssh_peer_image(
            r"C:\Users\ano\sshd.exe",
            r"C:\Windows"
        ));
        assert!(!trusted_ssh_peer_image(
            r"C:\fake\Windows\System32\OpenSSH\sshd.exe",
            r"C:\Windows"
        ));
    }

    #[test]
    fn release_plan_tracks_unique_successful_holds() {
        let mut held = HeldInputs::default();
        held.apply(InputTransition::VirtualKey {
            vk: 0x11,
            down: true,
        });
        held.apply(InputTransition::VirtualKey {
            vk: 0x11,
            down: true,
        });
        held.apply(InputTransition::Scancode {
            scancode: 0xE01D,
            down: true,
        });
        held.apply(InputTransition::MouseButton {
            button: MouseButton::Left,
            down: true,
        });

        assert_eq!(
            held.release_plan(),
            vec![
                InputTransition::VirtualKey {
                    vk: 0x11,
                    down: false
                },
                InputTransition::Scancode {
                    scancode: 0xE01D,
                    down: false,
                },
                InputTransition::MouseButton {
                    button: MouseButton::Left,
                    down: false,
                },
            ]
        );
        assert!(held.release_plan().is_empty());
    }

    #[test]
    fn successful_up_clears_only_the_matching_hold() {
        let mut held = HeldInputs::default();
        held.apply(InputTransition::VirtualKey {
            vk: 0x11,
            down: true,
        });
        held.apply(InputTransition::VirtualKey {
            vk: 0x12,
            down: true,
        });
        held.apply(InputTransition::VirtualKey {
            vk: 0x11,
            down: false,
        });

        assert_eq!(
            held.release_plan(),
            vec![InputTransition::VirtualKey {
                vk: 0x12,
                down: false
            }]
        );
    }
}

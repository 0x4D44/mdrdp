//! Portable bookkeeping for injected input that must be released on disconnect.

use crate::input_proto::MouseButton;

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
    use super::{HeldInputs, InputTransition};
    use crate::input_proto::MouseButton;

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

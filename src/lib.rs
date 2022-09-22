#[cfg(not(target_os = "windows"))]
compile_error!("Only supported on windows");

pub mod error;
pub mod keys;

use std::collections::HashMap;

use winapi::shared::windef::HWND;
use winapi::um::winuser;
use winapi::um::winuser::{
    GetAsyncKeyState, GetMessageW, RegisterHotKey, UnregisterHotKey, MSG, WM_HOTKEY,
};

use crate::{error::HkError, keys::*};

/// Identifier of a registered hotkey. This is returned when registering a hotkey and can be used
/// to unregister it again.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub struct HotkeyId(i32);

/// HotkeyCallback contains the callback function and a list of extra_keys.
///
struct HotkeyCallback<T> {
    /// Callback function to execute  when the hotkey matches
    callback: Box<dyn Fn() -> T + 'static>,
    /// List of additional VKs that are required to be pressed to execute
    /// the callback
    extra_keys: Vec<VKey>,
}

/// Register and manage hotkeys with windows, as well as the callbacks.
///
pub struct HotkeyManager<T> {
    id_offset: i32,
    handlers: HashMap<HotkeyId, HotkeyCallback<T>>,
}

impl<T> HotkeyManager<T> {
    /// Create a new HotkeyManager instance.
    ///
    /// The hotkey ids that are registered by this will start at offset 0,
    /// so creating a second instance with `new` will result in failing
    /// hotkey registration due to the ids being in use already. To register
    /// hotkeys with multiple instances see `new_with_id_offset`. Keep in
    /// mind though that only one instance can be listing for hotkeys anyways.
    ///
    pub fn new() -> HotkeyManager<T> {
        HotkeyManager {
            id_offset: 0,
            handlers: HashMap::new(),
        }
    }

    /// Create a new HotkeyManager instance and start enumerating the
    /// registered hotkey ids with `id_offset` to avoid id conflicts.
    ///
    /// This can be used to create multiple at instance of the `HotkeyManager`
    /// that all have hotkeys registered with windows.
    ///
    pub fn new_with_id_offset(id_offset: i32) -> HotkeyManager<T> {
        HotkeyManager {
            id_offset,
            handlers: HashMap::new(),
        }
    }

    /// Register a hotkey with callback and require additional extra keys to be pressed.
    ///
    /// This will try to register the hotkey&modifiers with windows and add the callback with
    /// the extra keys to the handlers.
    ///
    /// # Arguments
    ///
    /// * `key` - The main hotkey. For example VK_ENTER for CTRL + ALT + ENTER combination.
    ///
    /// * `key_modifiers` - The modifier keys as combined flags. This can be MOD_ALT, MOD_CONTROL,
    /// MOD_SHIFT or a bitwise combination of those. The modifier keys are the keys that need to
    /// be pressed in addition to the main hotkey in order for the hotkey event to fire.
    ///
    /// * `extra_keys` - A list of additional VKs that also need to be pressed for the hotkey callback
    /// to be executed. This is enforced after the windows hotkey event is fired but before executing
    /// the callback.
    ///
    /// * `callback` - A callback function or closure that will be executed when the hotkey is pressed
    ///
    /// # Windows API Functions used
    /// - https://docs.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registerhotkey
    ///
    pub fn register_extrakeys(
        &mut self,
        key: VKey,
        key_modifiers: &[ModKey],
        extra_keys: &[VKey],
        callback: impl Fn() -> T + 'static,
    ) -> Result<HotkeyId, HkError> {
        let register_id = HotkeyId(self.id_offset);
        self.id_offset += 1;

        // Try to register the hotkey combination with windows
        let reg_ok = unsafe {
            RegisterHotKey(
                0 as HWND,
                register_id.0,
                ModKey::combine(key_modifiers) | winuser::MOD_NOREPEAT as u32,
                key.to_vk_code() as u32,
            )
        };

        if reg_ok == 0 {
            Err(HkError::RegistrationFailed)
        } else {
            // Add the HotkeyCallback to the handlers when the hotkey was registered
            self.handlers.insert(
                register_id,
                HotkeyCallback {
                    callback: Box::new(callback),
                    extra_keys: extra_keys.to_owned(),
                },
            );

            Ok(register_id)
        }
    }

    /// Same as `register_extrakeys` but without extra keys.
    ///
    pub fn register(
        &mut self,
        key: VKey,
        key_modifiers: &[ModKey],
        callback: impl Fn() -> T + 'static,
    ) -> Result<HotkeyId, HkError> {
        self.register_extrakeys(key, key_modifiers, &[], callback)
    }

    pub fn unregister(&mut self, id: HotkeyId) -> Result<(), HkError> {
        let ok = unsafe { UnregisterHotKey(0 as HWND, id.0) };

        match ok {
            0 => Err(HkError::UnregistrationFailed),
            _ => {
                self.handlers.remove(&id);
                Ok(())
            }
        }
    }

    pub fn unregister_all(&mut self) -> Result<(), HkError> {
        let ids: Vec<_> = self.handlers.keys().copied().collect();
        for id in ids {
            self.unregister(id)?;
        }

        Ok(())
    }

    /// Poll a hotkey event, execute the callback if all keys match and return the callback
    /// result. If the event does not match all keys, None is returned.
    ///
    /// This will block until a hotkey is pressed and therefore not consume any cpu power.
    ///
    /// ## Windows API Functions used
    /// - https://docs.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getmessagew
    ///
    pub fn poll_event(&mut self) -> Option<T> {
        let mut msg = std::mem::MaybeUninit::<MSG>::uninit();

        // Block and read a message from the message queue. Filtered by only WM_HOTKEY messages
        let ok = unsafe { GetMessageW(msg.as_mut_ptr(), 0 as HWND, WM_HOTKEY, WM_HOTKEY) };

        if ok != 0 {
            let msg = unsafe { msg.assume_init() };

            if WM_HOTKEY == msg.message {
                let hk_id = HotkeyId(msg.wParam as i32);

                // Get the callback for the received ID
                if let Some(handler) = self.handlers.get(&hk_id) {
                    // Check if all extra keys are pressed
                    if let None = handler
                        .extra_keys
                        .iter()
                        .find(|&vk| !get_global_keystate(*vk))
                    {
                        return Some((handler.callback)());
                    }
                }
            }
        }

        None
    }

    pub fn event_loop(&mut self) {
        loop {
            self.poll_event();
        }
    }
}

/// Get the global keystate for a given Virtual Key.
///
/// Return true if the key is pressed, false otherwise.
///
/// ## Windows API Functions used
/// - https://docs.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getasynckeystate
///
pub fn get_global_keystate(vk: VKey) -> bool {
    // Most significant bit represents key state (1 => pressed, 0 => not pressed)
    let key_state = unsafe { GetAsyncKeyState(vk.to_vk_code()) };
    // Get most significant bit only
    let key_state = key_state as u32 >> 31;

    key_state == 1
}

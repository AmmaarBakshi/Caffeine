//! Lid switch state.
//!
//! Windows exposes no synchronous "is the lid closed?" query. Registering
//! for `GUID_LIDSWITCH_STATE_CHANGE` power-setting notifications makes the
//! system deliver the current state immediately, so we register a callback,
//! wait briefly for that first notification and unregister. If no
//! notification arrives the state is reported as unknown rather than guessed.

use std::ptr;
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Power::{
    DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, POWERBROADCAST_SETTING, PowerSettingRegisterNotification,
    PowerSettingUnregisterNotification,
};
use windows_sys::Win32::System::SystemServices::GUID_LIDSWITCH_STATE_CHANGE;
use windows_sys::Win32::UI::WindowsAndMessaging::{DEVICE_NOTIFY_CALLBACK, PBT_POWERSETTINGCHANGE};

use caffeine_core::Result;
use caffeine_core::power::LidState;

use super::util::{guid_to_u128, win_error};

static QUERY: Mutex<()> = Mutex::new(());
static SLOT: Mutex<Option<u32>> = Mutex::new(None);
static SIGNAL: Condvar = Condvar::new();

unsafe extern "system" fn on_lid_notification(
    _context: *const core::ffi::c_void,
    kind: u32,
    setting: *const core::ffi::c_void,
) -> u32 {
    if kind != PBT_POWERSETTINGCHANGE || setting.is_null() {
        return 0;
    }
    // SAFETY: for PBT_POWERSETTINGCHANGE `setting` points to a
    // POWERBROADCAST_SETTING followed by `DataLength` bytes of data.
    unsafe {
        let s = &*(setting as *const POWERBROADCAST_SETTING);
        if guid_to_u128(&s.PowerSetting) == guid_to_u128(&GUID_LIDSWITCH_STATE_CHANGE)
            && s.DataLength as usize >= std::mem::size_of::<u32>()
        {
            let value = ptr::read_unaligned(s.Data.as_ptr() as *const u32);
            if let Ok(mut slot) = SLOT.lock() {
                *slot = Some(value);
                SIGNAL.notify_all();
            }
        }
    }
    0
}

/// Query the lid switch; `timeout` bounds the wait for the initial
/// notification.
pub fn query(timeout: Duration) -> Result<LidState> {
    let _serial = QUERY.lock().unwrap_or_else(|p| p.into_inner());
    *SLOT.lock().unwrap_or_else(|p| p.into_inner()) = None;

    let mut params = DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
        Callback: Some(on_lid_notification),
        Context: ptr::null_mut(),
    };
    let mut handle: *mut core::ffi::c_void = ptr::null_mut();
    // SAFETY: `params` outlives the registration (we unregister below).
    let rc = unsafe {
        PowerSettingRegisterNotification(
            &GUID_LIDSWITCH_STATE_CHANGE,
            DEVICE_NOTIFY_CALLBACK,
            (&mut params as *mut DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS).cast(),
            &mut handle,
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(win_error("PowerSettingRegisterNotification(lid switch)", rc));
    }

    let slot = SLOT.lock().unwrap_or_else(|p| p.into_inner());
    let (slot, _) = SIGNAL
        .wait_timeout_while(slot, timeout, |v| v.is_none())
        .unwrap_or_else(|p| p.into_inner());
    let value = *slot;
    drop(slot);

    // SAFETY: unregistering the handle obtained above, exactly once.
    let rc = unsafe { PowerSettingUnregisterNotification(handle as isize) };
    if rc != ERROR_SUCCESS {
        log::warn!("PowerSettingUnregisterNotification failed with {rc}");
    }

    Ok(match value {
        Some(0) => LidState::Closed,
        Some(1) => LidState::Open,
        _ => LidState::Unknown,
    })
}

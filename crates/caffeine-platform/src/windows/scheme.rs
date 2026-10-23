//! Lid-close action in the active power scheme.
//!
//! The setting lives at `SUB_BUTTONS\LIDACTION` with values
//! 0 = do nothing, 1 = sleep, 2 = hibernate, 3 = shut down, separately for
//! AC ("plugged in") and DC ("on battery"). Changing it is persistent, so
//! every change is journaled with the original and applied values. Restoring
//! only reverts a value if it still equals what Caffeine applied, so a change
//! the user made in Settings in the meantime is never clobbered.

use std::ptr;

use serde::{Deserialize, Serialize};
use windows_sys::Win32::Foundation::{ERROR_SUCCESS, HLOCAL, LocalFree};
use windows_sys::Win32::System::Power::{
    PowerGetActiveScheme, PowerReadACValueIndex, PowerReadDCValueIndex, PowerReadFriendlyName,
    PowerSetActiveScheme, PowerWriteACValueIndex, PowerWriteDCValueIndex,
};
use windows_sys::Win32::System::SystemServices::{
    GUID_LIDCLOSE_ACTION, GUID_SYSTEM_BUTTON_SUBGROUP,
};
use windows_sys::core::GUID;

use caffeine_core::power::{ActiveAssertion, AwakeGuard, Capability, LidAction};
use caffeine_core::{Error, Result};

use super::util::{from_wide, guid_from_string, guid_to_string, win_error};
use crate::journal::{EntryKind, Journal, JournalEntry};

/// The active power scheme.
pub struct Scheme {
    pub guid: GUID,
    pub name: Option<String>,
}

pub fn active_scheme() -> Result<Scheme> {
    let mut ptr_guid: *mut GUID = ptr::null_mut();
    // SAFETY: on success the API allocates a GUID that we free with LocalFree.
    let rc = unsafe { PowerGetActiveScheme(ptr::null_mut(), &mut ptr_guid) };
    if rc != ERROR_SUCCESS || ptr_guid.is_null() {
        return Err(win_error("PowerGetActiveScheme", rc));
    }
    // SAFETY: ptr_guid points to a valid GUID allocated by the system.
    let guid = unsafe { *ptr_guid };
    // SAFETY: freeing the allocation returned above exactly once.
    unsafe { LocalFree(ptr_guid as HLOCAL) };
    Ok(Scheme {
        name: friendly_name(&guid),
        guid,
    })
}

fn friendly_name(scheme: &GUID) -> Option<String> {
    let mut size: u32 = 0;
    // SAFETY: first call queries the required buffer size.
    let rc = unsafe {
        PowerReadFriendlyName(
            ptr::null_mut(),
            scheme,
            ptr::null(),
            ptr::null(),
            ptr::null_mut(),
            &mut size,
        )
    };
    if rc != ERROR_SUCCESS || size == 0 {
        return None;
    }
    let mut buf = vec![0u16; (size as usize).div_ceil(2)];
    // SAFETY: buf holds at least `size` bytes.
    let rc = unsafe {
        PowerReadFriendlyName(
            ptr::null_mut(),
            scheme,
            ptr::null(),
            ptr::null(),
            buf.as_mut_ptr().cast(),
            &mut size,
        )
    };
    (rc == ERROR_SUCCESS).then(|| from_wide(&buf))
}

/// Raw AC/DC lid action indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LidValues {
    pub ac: u32,
    pub dc: u32,
}

pub fn read_lid_values(scheme: &GUID) -> Result<LidValues> {
    let mut ac = 0u32;
    let mut dc = 0u32;
    // SAFETY: all pointers are valid for the duration of the calls.
    let rc = unsafe {
        PowerReadACValueIndex(
            ptr::null_mut(),
            scheme,
            &GUID_SYSTEM_BUTTON_SUBGROUP,
            &GUID_LIDCLOSE_ACTION,
            &mut ac,
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(win_error("PowerReadACValueIndex(LIDACTION)", rc));
    }
    // SAFETY: as above.
    let rc = unsafe {
        PowerReadDCValueIndex(
            ptr::null_mut(),
            scheme,
            &GUID_SYSTEM_BUTTON_SUBGROUP,
            &GUID_LIDCLOSE_ACTION,
            &mut dc,
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(win_error("PowerReadDCValueIndex(LIDACTION)", rc));
    }
    Ok(LidValues { ac, dc })
}

/// Write lid action indices and re-apply the scheme so they take effect.
pub fn write_lid_values(scheme: &GUID, ac: Option<u32>, dc: Option<u32>) -> Result<()> {
    if let Some(ac) = ac {
        // SAFETY: pointers are valid for the call.
        let rc = unsafe {
            PowerWriteACValueIndex(
                ptr::null_mut(),
                scheme,
                &GUID_SYSTEM_BUTTON_SUBGROUP,
                &GUID_LIDCLOSE_ACTION,
                ac,
            )
        };
        if rc != ERROR_SUCCESS {
            return Err(win_error("PowerWriteACValueIndex(LIDACTION)", rc));
        }
    }
    if let Some(dc) = dc {
        // SAFETY: pointers are valid for the call.
        let rc = unsafe {
            PowerWriteDCValueIndex(
                ptr::null_mut(),
                scheme,
                &GUID_SYSTEM_BUTTON_SUBGROUP,
                &GUID_LIDCLOSE_ACTION,
                dc,
            )
        };
        if rc != ERROR_SUCCESS {
            return Err(win_error("PowerWriteDCValueIndex(LIDACTION)", rc));
        }
    }
    // SAFETY: re-activating the same scheme applies the modified values.
    let rc = unsafe { PowerSetActiveScheme(ptr::null_mut(), scheme) };
    if rc != ERROR_SUCCESS {
        return Err(win_error("PowerSetActiveScheme", rc));
    }
    Ok(())
}

pub fn action_from_index(index: u32) -> LidAction {
    match index {
        0 => LidAction::DoNothing,
        1 => LidAction::Sleep,
        2 => LidAction::Hibernate,
        3 => LidAction::ShutDown,
        other => LidAction::Other(format!("unknown ({other})")),
    }
}

pub fn index_from_action(action: &LidAction) -> Result<u32> {
    match action {
        LidAction::DoNothing => Ok(0),
        LidAction::Sleep => Ok(1),
        LidAction::Hibernate => Ok(2),
        LidAction::ShutDown => Ok(3),
        other => Err(Error::unsupported(format!(
            "Windows does not offer the lid action \"{other}\" (choose do-nothing, sleep, hibernate or shut-down)"
        ))),
    }
}

/// Journal payload for lid changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LidJournalPayload {
    pub scheme: String,
    pub scheme_name: Option<String>,
    pub original: LidValues,
    pub applied: LidValues,
}

impl LidJournalPayload {
    pub fn from_entry(entry: &JournalEntry) -> Result<LidJournalPayload> {
        serde_json::from_value(entry.payload.clone()).map_err(|e| {
            Error::failure(format!(
                "restore journal entry {} is corrupt: {e}",
                entry.id
            ))
        })
    }
}

/// Restore one journaled lid change. Values the user changed since Caffeine
/// applied them are left alone; a note explains what was skipped.
pub fn restore_entry(entry: &JournalEntry) -> Result<String> {
    let payload = LidJournalPayload::from_entry(entry)?;
    let scheme = guid_from_string(&payload.scheme).ok_or_else(|| {
        Error::failure(format!("invalid scheme GUID {} in journal", payload.scheme))
    })?;
    let current = read_lid_values(&scheme)?;
    let ac = (current.ac == payload.applied.ac && current.ac != payload.original.ac)
        .then_some(payload.original.ac);
    let dc = (current.dc == payload.applied.dc && current.dc != payload.original.dc)
        .then_some(payload.original.dc);
    if ac.is_some() || dc.is_some() {
        write_lid_values(&scheme, ac, dc)?;
    }
    let mut notes = Vec::new();
    if current.ac != payload.applied.ac {
        notes.push("AC value was changed by someone else; left unchanged");
    }
    if current.dc != payload.applied.dc {
        notes.push("battery value was changed by someone else; left unchanged");
    }
    let name = payload.scheme_name.unwrap_or(payload.scheme);
    Ok(format!(
        "restored lid-close action in power scheme \"{name}\" to plugged in: {}, on battery: {}{}",
        action_from_index(payload.original.ac),
        action_from_index(payload.original.dc),
        if notes.is_empty() {
            String::new()
        } else {
            format!(" ({})", notes.join("; "))
        }
    ))
}

/// Apply new lid values to the active scheme, journaling the originals.
/// Returns the journal entry id.
pub fn apply_with_journal(
    journal: &Journal,
    kind: EntryKind,
    ac: Option<u32>,
    dc: Option<u32>,
) -> Result<(String, Scheme, LidValues)> {
    let scheme = active_scheme()?;
    let current = read_lid_values(&scheme.guid)?;
    // For persistent changes keep the earliest original values, so repeated
    // `lid set` calls still restore to the pre-Caffeine state.
    let existing = match &kind {
        EntryKind::PersistentLidPolicy => journal.find(&kind)?,
        EntryKind::SessionLidOverride { .. } => None,
    };
    let original = match &existing {
        Some(e) => LidJournalPayload::from_entry(e)?.original,
        None => current,
    };
    let applied = LidValues {
        ac: ac.unwrap_or(current.ac),
        dc: dc.unwrap_or(current.dc),
    };
    let payload = LidJournalPayload {
        scheme: guid_to_string(&scheme.guid),
        scheme_name: scheme.name.clone(),
        original,
        applied,
    };
    let description = format!(
        "lid-close action in \"{}\" changed from plugged in: {}, on battery: {}",
        scheme.name.as_deref().unwrap_or("active scheme"),
        action_from_index(original.ac),
        action_from_index(original.dc)
    );
    // Journal first: if writing the setting fails, the entry is removed; if
    // we crash after writing it, the entry allows recovery.
    let id = journal.add(
        kind,
        &description,
        serde_json::to_value(&payload).expect("payload serializes"),
    )?;
    if let Err(e) = write_lid_values(&scheme.guid, ac, dc) {
        let _ = journal.remove(&id);
        return Err(e);
    }
    if let Some(old) = existing {
        let _ = journal.remove(&old.id);
    }
    Ok((id, scheme, applied))
}

/// Session-scoped lid override: restores the journaled values on release.
pub struct SessionLidOverride {
    journal: Journal,
    entry_id: String,
    description: String,
    released: bool,
}

impl SessionLidOverride {
    pub fn apply(
        journal: &Journal,
        ac: Option<u32>,
        dc: Option<u32>,
    ) -> Result<SessionLidOverride> {
        let kind = EntryKind::SessionLidOverride {
            pid: std::process::id(),
        };
        let (entry_id, scheme, applied) = apply_with_journal(journal, kind, ac, dc)?;
        Ok(SessionLidOverride {
            journal: journal.clone(),
            entry_id,
            description: format!(
                "power scheme \"{}\" temporarily set to plugged in: {}, on battery: {}",
                scheme.name.unwrap_or_default(),
                action_from_index(applied.ac),
                action_from_index(applied.dc)
            ),
            released: false,
        })
    }

    fn restore(&mut self) -> Result<()> {
        if self.released {
            return Ok(());
        }
        self.released = true;
        let entry = self
            .journal
            .entries()?
            .into_iter()
            .find(|e| e.id == self.entry_id);
        if let Some(entry) = entry {
            let note = restore_entry(&entry)?;
            log::info!("{note}");
            self.journal.remove(&entry.id)?;
        }
        Ok(())
    }
}

impl AwakeGuard for SessionLidOverride {
    fn assertions(&self) -> Vec<ActiveAssertion> {
        vec![ActiveAssertion {
            capability: Capability::LidSessionOverride,
            mechanism: "power scheme LIDACTION (journaled)".into(),
            detail: self.description.clone(),
        }]
    }

    fn release(mut self: Box<Self>) -> Result<()> {
        self.restore()
    }
}

impl Drop for SessionLidOverride {
    fn drop(&mut self) {
        if let Err(e) = self.restore() {
            log::error!("restoring lid-close action: {e}; run `caffeine lid restore`");
        }
    }
}

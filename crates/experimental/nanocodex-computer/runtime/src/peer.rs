//! Configurable socket signing gate. Uses the connected socket's audit token,
//! never a caller-supplied PID. The allowlist belongs to this independent system.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::Read;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub team_ids: Vec<String>,
    pub signing_identifiers: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Identity {
    pub team_id: String,
    pub signing_identifier: String,
}
impl Policy {
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 1024 * 1024 {
            return Err(Error::invalid("Peer policy exceeds 1 MiB"));
        }
        let value: Self = serde_json::from_slice(&bytes)?;
        if value.team_ids.is_empty() || value.signing_identifiers.is_empty() {
            return Err(Error::invalid("Peer identity allowlists must be nonempty"));
        }
        Ok(value)
    }
    pub fn decide(&self, identities: [Option<Identity>; 3]) -> Value {
        let reason = if identities.iter().any(|i| {
            i.as_ref()
                .is_none_or(|i| i.team_id.is_empty() || i.signing_identifier.is_empty())
        }) {
            Some("missing-code-signing-identity")
        } else if identities.iter().flatten().any(|i| {
            !self.team_ids.contains(&i.team_id)
                || !self.signing_identifiers.contains(&i.signing_identifier)
        }) {
            Some("untrusted-code-signing-identity")
        } else {
            None
        };
        let mut result = json!({"authorized":reason.is_none()});
        if let Some(reason) = reason {
            result["reason"] = json!(reason);
        }
        if let Some(peer) = &identities[0] {
            if !peer.team_id.is_empty() {
                result["teamId"] = json!(peer.team_id);
            }
            if !peer.signing_identifier.is_empty() {
                result["signingIdentifier"] = json!(peer.signing_identifier);
            }
        }
        result
    }
    pub fn authorize_with(
        &self,
        mut lookup: impl FnMut(u8) -> Result<Option<Identity>>,
    ) -> Result<Value> {
        // Read errors take precedence over missing identities at earlier depths.
        let peer = lookup(0)?;
        let parent = lookup(1)?;
        let grandparent = lookup(2)?;
        Ok(self.decide([peer, parent, grandparent]))
    }
    #[cfg(unix)]
    pub fn authorize_socket(&self, fd: std::os::fd::RawFd) -> Result<Value> {
        #[cfg(target_os = "macos")]
        {
            let token = macos::token(fd)?;
            self.authorize_with(|depth| macos::identity_at(&token, depth))
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = fd;
            Err(Error::unsupported(
                "Code-signing peer policy requires macOS; same-UID socket gating remains available",
            ))
        }
    }
}
#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use core_foundation::{
        base::{CFType, CFTypeRef, TCFType},
        data::CFData,
        dictionary::{CFDictionary, CFDictionaryRef},
        number::CFNumber,
        string::{CFString, CFStringRef},
    };
    use std::{ffi::c_void, ptr};
    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        static kSecGuestAttributeAudit: CFStringRef;
        static kSecGuestAttributePid: CFStringRef;
        static kSecCodeInfoFlags: CFStringRef;
        static kSecCodeInfoIdentifier: CFStringRef;
        static kSecCodeInfoTeamIdentifier: CFStringRef;
        fn SecCodeCopyGuestWithAttributes(
            host: CFTypeRef,
            attributes: CFDictionaryRef,
            flags: u32,
            guest: *mut CFTypeRef,
        ) -> i32;
        fn SecCodeCopySigningInformation(
            code: CFTypeRef,
            flags: u32,
            information: *mut CFDictionaryRef,
        ) -> i32;
    }
    unsafe extern "C" {
        fn proc_pidinfo(pid: i32, flavor: i32, arg: u64, buffer: *mut c_void, size: i32) -> i32;
    }
    pub fn token(fd: i32) -> Result<[u32; 8]> {
        if fd < 0 {
            return Err(Error::invalid("Invalid socket descriptor"));
        }
        let mut token = [0u32; 8];
        let mut length = 32u32;
        let result = unsafe { libc::getsockopt(fd, 0, 6, token.as_mut_ptr() as _, &mut length) };
        if result != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if length != 32 {
            return Err(Error::action("Unexpected peer audit token length"));
        }
        Ok(token)
    }
    pub fn identity_at(token: &[u32; 8], depth: u8) -> Result<Option<Identity>> {
        let (key, value) = if depth == 0 {
            let bytes = unsafe { std::slice::from_raw_parts(token.as_ptr() as *const u8, 32) };
            (
                unsafe { CFString::wrap_under_get_rule(kSecGuestAttributeAudit) },
                CFData::from_buffer(bytes).as_CFType(),
            )
        } else {
            let mut pid = token[5] as i32;
            for _ in 0..depth {
                if pid < 2 {
                    return Ok(None);
                }
                let mut bsd = [0u32; 34];
                let count = unsafe { proc_pidinfo(pid, 3, 0, bsd.as_mut_ptr() as _, 136) };
                if count != 136 {
                    return Ok(None);
                }
                pid = bsd[4] as i32;
            }
            if pid < 2 {
                return Ok(None);
            }
            (
                unsafe { CFString::wrap_under_get_rule(kSecGuestAttributePid) },
                CFNumber::from(pid).as_CFType(),
            )
        };
        let attributes = CFDictionary::from_CFType_pairs(&[(key, value)]);
        let mut guest = ptr::null();
        let status = unsafe {
            SecCodeCopyGuestWithAttributes(
                ptr::null(),
                attributes.as_concrete_TypeRef(),
                0,
                &mut guest,
            )
        };
        if status != 0 || guest.is_null() {
            return Err(Error::action(format!(
                "Cannot read peer signing identity at depth {depth} ({status})"
            )));
        }
        let guest = unsafe { CFType::wrap_under_create_rule(guest) };
        let mut info = ptr::null();
        let status = unsafe { SecCodeCopySigningInformation(guest.as_CFTypeRef(), 2, &mut info) };
        if status != 0 || info.is_null() {
            return Err(Error::action(format!(
                "Cannot copy peer signing information at depth {depth} ({status})"
            )));
        }
        let info: CFDictionary<CFString, CFType> =
            unsafe { CFDictionary::wrap_under_create_rule(info) };
        let key = unsafe { CFString::wrap_under_get_rule(kSecCodeInfoFlags) };
        if info
            .find(&key)
            .and_then(|v| v.downcast::<CFNumber>())
            .and_then(|v| v.to_i64())
            .is_some_and(|flags| flags & 2 != 0)
        {
            return Ok(None);
        }
        let read = |raw: CFStringRef| -> Result<Option<String>> {
            let key = unsafe { CFString::wrap_under_get_rule(raw) };
            let value = info
                .find(&key)
                .and_then(|v| v.downcast::<CFString>())
                .map(|v| v.to_string());
            if value
                .as_ref()
                .is_some_and(|s| s.len() > 1023 || s.contains('\0'))
            {
                return Err(Error::action("Peer signing identity exceeds string limits"));
            }
            Ok(value)
        };
        let Some(team_id) = read(unsafe { kSecCodeInfoTeamIdentifier })? else {
            return Ok(None);
        };
        let Some(signing_identifier) = read(unsafe { kSecCodeInfoIdentifier })? else {
            return Ok(None);
        };
        Ok(Some(Identity {
            team_id,
            signing_identifier,
        }))
    }
}

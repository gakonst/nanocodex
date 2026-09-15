//! Bounded metadata resolution for AX attributed-text element references.
//! These values are AXUIElements, not strings containing a URL or a file path.
use super::Ax;
use crate::{Error, Result};
use accessibility_sys::{AXUIElementGetPid, AXUIElementGetTypeID};
use core_foundation::{
    base::{CFEqual, CFType, TCFType},
    string::CFString,
    url::CFURL,
};
use std::time::Instant;

const MAX_REFERENCES: usize = 256;
const MAX_METADATA_UNITS: isize = 16 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Metadata {
    pub role: Option<String>,
    pub role_description: Option<String>,
    pub description: Option<String>,
    pub url: Option<String>,
}
impl Metadata {
    fn read(element: &Ax, deadline: Instant) -> Result<Self> {
        let get = |name| {
            if Instant::now() >= deadline {
                return Err(Error::action("Attributed reference deadline exceeded"));
            }
            element.get(name)
        };
        fn string(value: Option<CFType>, name: &str, url: bool) -> Result<Option<String>> {
            let Some(value) = value else { return Ok(None) };
            let text = if let Some(text) = value.downcast::<CFString>() {
                text
            } else if url && let Some(value) = value.downcast::<CFURL>() {
                value.get_string()
            } else {
                return Err(Error::action(format!(
                    "Attributed reference {name} has invalid native type"
                )));
            };
            if text.char_len() > MAX_METADATA_UNITS {
                return Err(Error::action(
                    "Attributed reference metadata exceeds length bound",
                ));
            }
            Ok(Some(text.to_string()))
        }
        Ok(Self {
            role: string(get("AXRole")?, "AXRole", false)?,
            role_description: string(get("AXRoleDescription")?, "AXRoleDescription", false)?,
            description: string(get("AXDescription")?, "AXDescription", false)?,
            url: string(get("AXURL")?, "AXURL", true)?,
        })
    }
}

pub(super) struct ReferenceCache {
    expected_pid: i32,
    deadline: Instant,
    entries: Vec<(Ax, Metadata)>,
}
impl ReferenceCache {
    pub fn new(expected_pid: i32, deadline: Instant) -> Self {
        Self {
            expected_pid,
            deadline,
            entries: Vec::new(),
        }
    }
    pub fn resolve(&mut self, value: CFType) -> Result<Metadata> {
        let deadline = self.deadline;
        self.resolve_with(value, |element| {
            super::ax_ok(unsafe {
                accessibility_sys::AXUIElementSetMessagingTimeout(element.ptr(), 0.25)
            })?;
            Metadata::read(element, deadline)
        })
    }
    fn resolve_with(
        &mut self,
        value: CFType,
        read: impl FnOnce(&Ax) -> Result<Metadata>,
    ) -> Result<Metadata> {
        self.check()?;
        if value.type_of() != unsafe { AXUIElementGetTypeID() } {
            return Err(Error::action("Attributed reference is not an AX element"));
        }
        let element = Ax(value);
        let mut pid = 0;
        super::ax_ok(unsafe { AXUIElementGetPid(element.ptr(), &mut pid) })?;
        if pid <= 0 || pid != self.expected_pid {
            return Err(Error::action(
                "Attributed reference belongs to another application",
            ));
        }
        if let Some((_, metadata)) = self.entries.iter().find(|(cached, _)| unsafe {
            CFEqual(cached.0.as_CFTypeRef(), element.0.as_CFTypeRef()) != 0
        }) {
            return Ok(metadata.clone());
        }
        if self.entries.len() >= MAX_REFERENCES {
            return Err(Error::action(
                "Attributed references exceed observation bound",
            ));
        }
        // Publish only a fully successful metadata read. Failed/native-denied
        // reads are never memoized as missing metadata or successful rendering.
        let metadata = read(&element)?;
        self.check()?;
        self.entries.push((element, metadata.clone()));
        Ok(metadata)
    }
    pub fn check(&self) -> Result<()> {
        if Instant::now() >= self.deadline {
            Err(Error::action("Attributed reference deadline exceeded"))
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use accessibility_sys::AXUIElementCreateApplication;
    fn application(pid: i32) -> CFType {
        // Creates a local CF reference only. No AX attributes/UI are observed.
        unsafe { CFType::wrap_under_create_rule(AXUIElementCreateApplication(pid).cast()) }
    }
    #[test]
    fn references_require_elements_and_approved_pid_before_metadata_reads() {
        let pid = std::process::id() as i32;
        let mut cache =
            ReferenceCache::new(pid, Instant::now() + std::time::Duration::from_secs(5));
        assert!(
            cache
                .resolve_with(
                    CFString::new("https://example.invalid").as_CFType(),
                    |_| panic!("must not read")
                )
                .unwrap_err()
                .message
                .contains("not an AX element")
        );
        assert!(
            cache
                .resolve_with(application(pid + 1), |_| panic!("must not read"))
                .unwrap_err()
                .message
                .contains("another application")
        );
        assert!(cache.entries.is_empty());
    }
    #[test]
    fn references_cache_by_native_identity_and_do_not_publish_failed_reads() {
        let pid = std::process::id() as i32;
        let mut cache =
            ReferenceCache::new(pid, Instant::now() + std::time::Duration::from_secs(5));
        let reference = application(pid);
        assert!(
            cache
                .resolve_with(reference.clone(), |_| Err(Error::action("fixture denied")))
                .is_err()
        );
        assert!(cache.entries.is_empty());
        let expected = Metadata {
            role: Some("AXLink".into()),
            url: Some("https://example.invalid/".into()),
            ..Default::default()
        };
        assert_eq!(
            cache
                .resolve_with(reference, |_| Ok(expected.clone()))
                .unwrap(),
            expected
        );
        assert_eq!(
            cache
                .resolve_with(application(pid), |_| panic!(
                    "cached identity must not refetch"
                ))
                .unwrap(),
            expected
        );
        assert_eq!(cache.entries.len(), 1);
    }
    #[test]
    fn references_expired_reader_result_is_never_published() {
        let pid = std::process::id() as i32;
        let mut cache =
            ReferenceCache::new(pid, Instant::now() + std::time::Duration::from_millis(20));
        let result = cache.resolve_with(application(pid), |_| {
            std::thread::sleep(std::time::Duration::from_millis(30));
            Ok(Metadata::default())
        });
        assert!(result.unwrap_err().message.contains("deadline exceeded"));
        assert!(cache.entries.is_empty());
        assert!(
            cache
                .resolve_with(application(pid), |_| panic!("expired reader must not run"))
                .is_err()
        );
    }
}

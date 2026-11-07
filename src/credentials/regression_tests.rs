use super::*;
use crate::model::{HostProfile, SuSettings, SudoSettings};

struct Fixture {
    host: HostProfile,
    slots: Vec<CredentialKind>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            host: HostProfile {
                username: "alice".into(),
                ..Default::default()
            },
            slots: vec![CredentialKind::Password, CredentialKind::KeyPassphrase],
        }
    }

    fn login_slot(&mut self) -> CredentialKind {
        let kind = CredentialKind::LoginPassword(Uuid::new_v4());
        self.slots.push(kind);
        kind
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for &kind in &self.slots {
            delete(self.host.id, kind).unwrap();
        }
        for &kind in &self.slots {
            assert!(
                load(self.host.id, kind).unwrap().is_none(),
                "temporary slot not removed: {} {kind:?}",
                self.host.id
            );
        }
    }
}

#[test]
fn versioned_login_never_falls_back_to_the_host_slot() {
    let _guard = crate::credentials::test_lock();
    let mut fixture = Fixture::new();
    let slot = fixture.login_slot();
    store(fixture.host.id, CredentialKind::Password, "host-fixture").unwrap();
    assert!(load(fixture.host.id, slot).unwrap().is_none());
    store(fixture.host.id, slot, "identity-fixture").unwrap();
    assert_eq!(
        load(fixture.host.id, slot).unwrap().unwrap().as_str(),
        "identity-fixture"
    );
    delete(fixture.host.id, slot).unwrap();
    assert!(load(fixture.host.id, slot).unwrap().is_none());
}

#[test]
fn failed_identity_rotation_restores_host_or_versioned_secret_without_new_slot() {
    let _guard = crate::credentials::test_lock();
    for versioned in [false, true] {
        let mut fixture = Fixture::new();
        let old = if versioned {
            fixture.login_slot()
        } else {
            CredentialKind::Password
        };
        let new = fixture.login_slot();
        store(fixture.host.id, old, "old-fixture").unwrap();
        let result = save_transaction(
            fixture.host.id,
            &[
                (old, None),
                (new, Some(Zeroizing::new("new-fixture".into()))),
            ],
            || Err("metadata conflict fixture".into()),
        );
        assert!(result.unwrap_err().contains("metadata conflict"));
        assert_eq!(
            load(fixture.host.id, old).unwrap().unwrap().as_str(),
            "old-fixture"
        );
        assert!(load(fixture.host.id, new).unwrap().is_none());
    }
}

#[test]
fn stale_sudo_and_su_profiles_never_read_new_login_identity() {
    let _guard = crate::credentials::test_lock();
    for versioned in [false, true] {
        let mut fixture = Fixture::new();
        fixture.host.advanced.sudo = Some(SudoSettings {
            enabled: true,
            ..Default::default()
        });
        fixture.host.advanced.su = Some(SuSettings { enabled: true });
        if versioned {
            let CredentialKind::LoginPassword(id) = fixture.login_slot() else {
                unreachable!()
            };
            fixture.host.login_password_id = Some(id);
        }
        let old = login_password_kind(&fixture.host);
        store(fixture.host.id, old, "alice-fixture").unwrap();
        let new = fixture.login_slot();
        save_transaction(
            fixture.host.id,
            &[
                (old, None),
                (new, Some(Zeroizing::new("bob-fixture".into()))),
            ],
            || Ok(()),
        )
        .unwrap();
        assert_eq!(
            crate::sudo::password_for(&fixture.host, "alice")
                .unwrap_err()
                .code,
            "SUDO_CREDENTIAL_MISSING"
        );
        assert_eq!(
            crate::su::password_for(&fixture.host, "alice")
                .unwrap_err()
                .code,
            "SU_CREDENTIAL_MISSING"
        );
        let mut bob = fixture.host.clone();
        let CredentialKind::LoginPassword(id) = new else {
            unreachable!()
        };
        bob.login_password_id = Some(id);
        bob.username = "bob".into();
        assert_eq!(
            crate::sudo::password_for(&bob, "bob").unwrap().as_str(),
            "bob-fixture"
        );
        assert_eq!(
            crate::su::password_for(&bob, "bob").unwrap().as_str(),
            "bob-fixture"
        );
    }
}

#[test]
fn versioned_snapshot_restore_and_serde_keep_exact_login_slot() {
    let _guard = crate::credentials::test_lock();
    let mut fixture = Fixture::new();
    let CredentialKind::LoginPassword(id) = fixture.login_slot() else {
        unreachable!()
    };
    fixture.host.login_password_id = Some(id);
    let json = serde_json::to_value(&fixture.host).unwrap();
    let host: HostProfile = serde_json::from_value(json).unwrap();
    assert_eq!(host.login_password_id, Some(id));
    let old: HostProfile = serde_json::from_str("{}").unwrap();
    assert_eq!(old.login_password_id, None);
    let slot = login_password_kind(&host);
    store(host.id, slot, "snapshot-fixture").unwrap();
    let snapshot = snapshot_host(&host).unwrap();
    delete_host(&host).unwrap();
    assert!(load(host.id, slot).unwrap().is_none());
    restore(host.id, &snapshot).unwrap();
    assert_eq!(
        load(host.id, slot).unwrap().unwrap().as_str(),
        "snapshot-fixture"
    );
    assert!(load(host.id, CredentialKind::Password).unwrap().is_none());
}

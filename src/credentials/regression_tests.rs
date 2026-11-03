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

    fn legacy(&self) -> Entry {
        Entry::new(LEGACY_SERVICE, &self.host.id.to_string()).unwrap()
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
        assert!(matches!(
            self.legacy().get_password(),
            Err(KeyringError::NoEntry)
        ));
    }
}

#[test]
fn legacy_migration_removes_source_and_deleted_password_stays_deleted() {
    let _guard = crate::credentials::test_lock();
    let fixture = Fixture::new();
    fixture.legacy().set_password("legacy-fixture").unwrap();
    let migrated = load(fixture.host.id, CredentialKind::Password)
        .unwrap()
        .unwrap();
    assert_eq!(migrated.as_str(), "legacy-fixture");
    assert!(matches!(
        fixture.legacy().get_password(),
        Err(KeyringError::NoEntry)
    ));
    delete(fixture.host.id, CredentialKind::Password).unwrap();
    assert!(
        load(fixture.host.id, CredentialKind::Password)
            .unwrap()
            .is_none()
    );
}

#[test]
fn explicit_delete_removes_unmigrated_and_shadowed_legacy_passwords() {
    let _guard = crate::credentials::test_lock();
    for current in [false, true] {
        let fixture = Fixture::new();
        fixture.legacy().set_password("obsolete-fixture").unwrap();
        if current {
            store(fixture.host.id, CredentialKind::Password, "current-fixture").unwrap();
        }
        delete(fixture.host.id, CredentialKind::Password).unwrap();
        assert!(
            load(fixture.host.id, CredentialKind::Password)
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            fixture.legacy().get_password(),
            Err(KeyringError::NoEntry)
        ));
    }
}

#[test]
fn versioned_login_never_falls_back_to_host_or_legacy_slots() {
    let _guard = crate::credentials::test_lock();
    let mut fixture = Fixture::new();
    let slot = fixture.login_slot();
    fixture.legacy().set_password("legacy-fixture").unwrap();
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
fn failed_identity_rotation_restores_legacy_or_versioned_secret_without_new_slot() {
    let _guard = crate::credentials::test_lock();
    for versioned in [false, true] {
        let mut fixture = Fixture::new();
        let old = if versioned {
            fixture.login_slot()
        } else {
            CredentialKind::Password
        };
        let new = fixture.login_slot();
        if versioned {
            store(fixture.host.id, old, "old-fixture").unwrap();
        } else {
            fixture.legacy().set_password("old-fixture").unwrap();
        }
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
        assert!(matches!(
            fixture.legacy().get_password(),
            Err(KeyringError::NoEntry)
        ));
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

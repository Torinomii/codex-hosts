use crate::model::{HostProfile, Protocol};
use crate::ssh::{self, OperationLimits, RemoteFailure, RemoteManyResult, RemoteResult};
use crate::telnet;

pub fn probe(
    profile: &HostProfile,
    hosts: &[HostProfile],
    limits: OperationLimits,
) -> Result<RemoteResult, RemoteFailure> {
    match profile.protocol {
        Protocol::Ssh => ssh::probe(profile, hosts, limits),
        Protocol::Telnet => telnet::probe(profile, limits),
    }
}

pub fn execute(
    profile: &HostProfile,
    hosts: &[HostProfile],
    command: &str,
    limits: OperationLimits,
) -> Result<RemoteResult, RemoteFailure> {
    execute_with_input(profile, hosts, command, None, limits)
}

pub fn execute_with_input(
    profile: &HostProfile,
    hosts: &[HostProfile],
    command: &str,
    stdin: Option<&str>,
    limits: OperationLimits,
) -> Result<RemoteResult, RemoteFailure> {
    ssh::validate_stdin(stdin)?;
    match profile.protocol {
        Protocol::Ssh => ssh::execute_with_input(profile, hosts, command, stdin, limits),
        Protocol::Telnet if stdin.is_some() => Err(RemoteFailure::new(
            "STDIN_UNSUPPORTED",
            "Optional stdin requires an SSH host; Telnet has no separate command input channel.",
        )),
        Protocol::Telnet => telnet::execute(profile, command, limits),
    }
}

pub fn execute_many(
    profile: &HostProfile,
    hosts: &[HostProfile],
    commands: &[String],
    max_concurrency: usize,
    limits: OperationLimits,
) -> Result<RemoteManyResult, RemoteFailure> {
    match profile.protocol {
        Protocol::Ssh => ssh::execute_many(profile, hosts, commands, max_concurrency, limits),
        Protocol::Telnet => Err(RemoteFailure::new(
            "SSH_REQUIRED",
            "exec_many is available only for SSH hosts.",
        )),
    }
}

pub async fn probe_async(
    profile: &HostProfile,
    hosts: &[HostProfile],
    limits: OperationLimits,
) -> Result<RemoteResult, RemoteFailure> {
    execute_with_input_async(profile, hosts, "hostname", None, limits).await
}

pub async fn execute_with_input_async(
    profile: &HostProfile,
    hosts: &[HostProfile],
    command: &str,
    stdin: Option<&str>,
    limits: OperationLimits,
) -> Result<RemoteResult, RemoteFailure> {
    ssh::validate_stdin(stdin)?;
    match profile.protocol {
        Protocol::Ssh => {
            ssh::execute_with_input_async(profile, hosts, command, stdin, limits).await
        }
        Protocol::Telnet if stdin.is_some() => Err(RemoteFailure::new(
            "STDIN_UNSUPPORTED",
            "Optional stdin requires an SSH host; Telnet has no separate command input channel.",
        )),
        Protocol::Telnet => telnet::execute_bounded(profile, command, limits).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provided_telnet_input_fails_before_credentials_or_network() {
        let host = HostProfile {
            protocol: Protocol::Telnet,
            ..Default::default()
        };
        for input in ["", "data"] {
            let error =
                execute_with_input(&host, &[], "cat", Some(input), OperationLimits::default())
                    .unwrap_err();
            assert_eq!(error.code, "STDIN_UNSUPPORTED");
        }
    }
}

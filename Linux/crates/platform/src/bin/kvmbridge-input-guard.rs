//! Privileged helper that pauses this computer's keyboards and mice while
//! another computer controls it. Started by KVM Bridge through pkexec; see
//! `kvmbridge_platform::linux::guard`.

#[cfg(target_os = "linux")]
fn main() {
    std::process::exit(kvmbridge_platform::linux::guard::helper_main());
}

#[cfg(not(target_os = "linux"))]
fn main() {}

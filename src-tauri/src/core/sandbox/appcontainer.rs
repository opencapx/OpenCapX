//! Windows AppContainer backend — the third OS guard for `opencapx sandbox` (W1).
//!
//! AppContainer is the kernel primitive Edge/Chrome already use: a "lowbox" token built from an
//! AppContainer SID that carries **no user SID**, so it can only reach what an ACL explicitly
//! grants it. Same philosophy as seatbelt and bubblewrap — an OS primitive fence against
//! accidents, not a boundary against a determined attacker — and no admin rights, no
//! virtualization, no new runtime dependency.
//!
//! A run is assembled in three steps:
//!
//! 1. ensure the `OpenCapX.Sandbox` AppContainer profile exists (it is persisted in the
//!    registry and reused; only the first run creates it) and derive its SID;
//! 2. grant that SID access to the scratch dir and the `--rw` dirs — installer mode also grants
//!    `$HOME`, then layers explicit deny ACEs over the secret/persistence list — remembering
//!    every path so the ACEs can be **removed again when the run ends**;
//! 3. `CreateProcessW` with a `SECURITY_CAPABILITIES` proc-thread attribute, started suspended,
//!    assigned to a Job Object, then resumed.
//!
//! The network needs no ACL work: an AppContainer token with no capabilities cannot create a
//! socket at all. That is the strict-mode fence, and the well-known `internetClient` capability
//! SID (`S-1-15-3-1`) is what installer mode (or `--allow-net`) adds back.
//!
//! ## Known deviations from the seatbelt / bwrap calibration
//!
//! - **Reads are fenced too, not just writes.** A lowbox token has no user SID, so anything
//!   under the user profile is unreadable unless it is granted. That is *stricter* than the
//!   "reads globally allowed" header the macOS profile is calibrated to; a strict-mode command
//!   that needs to read its own config will fail. If that shows up in practice, v1.1 adds a
//!   read-only grant.
//! - **A second writable area always exists**: the profile's own package folder under
//!   `%LOCALAPPDATA%\Packages\OpenCapX.Sandbox`, which the lowbox can always write.
//! - **Inherited ACEs outlive the run on fresh artifacts.** Grants inherit, so a file the
//!   sandbox *creates* inside a granted tree keeps the sandbox SID on its own DACL afterwards.
//!   The granted directories are fenced again (see `Grants`), the new files are not swept —
//!   walking a user directory tree after every run is not affordable, and the user owns those
//!   files anyway.
//! - **No LSASS / kernel isolation** — same tier as the other two backends.
//! - The shared deny list is enforced **wholesale, not per read/write**: a lowbox grant is
//!   all-or-nothing (`GENERIC_ALL`), so there is no way to take away only the reads on `~/.ssh`
//!   and leave the writes. Both halves get a `GENERIC_ALL` deny; `DenyKind` records where each
//!   path came from, for the review artifact.
//! - `INSTALLER_DENY_WRITE_ABS` (macOS absolute system paths) has no Windows mapping: the
//!   installer grant only ever reaches `$HOME`, so `C:\Windows` and `Program Files` are outside
//!   it by construction.

use super::*;

/// Stable profile name. Persisted under
/// `HKCU\Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppContainer\Mappings`
/// and reused across runs, so its SID — and therefore every ACL we write — stays valid.
pub(crate) const PROFILE_NAME: &str = "OpenCapX.Sandbox";

/// Well-known AppContainer capability SID for outbound internet access. Without it a lowbox
/// token has no sockets at all, which is exactly the strict-mode fence.
pub(crate) const INTERNET_CLIENT_SID: &str = "S-1-15-3-1";

/// Reason reported when the profile cannot be created or derived — a group policy, a locked-down
/// session, or a Windows build without userenv. The sandbox layer stays fail-open on it.
pub(crate) const UNAVAILABLE: &str =
    "AppContainer profile unavailable (group policy, locked-down session, or no userenv)";

/// Which half of the shared installer deny list a Windows path was mapped from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DenyKind {
    /// Secret material an installer has no business reading (and, here, not writing either).
    Secret,
    /// Persistence hot spots: surviving a reboot is the thing being closed off.
    Persist,
}

/// Capabilities handed to the token. Installer mode needs the network to install anything;
/// strict mode only gets it when `--allow-net` was passed. An empty list is the fence.
pub(crate) fn capabilities_for(mode: Mode, allow_net: bool) -> &'static [&'static str] {
    if mode == Mode::Installer || allow_net {
        &[INTERNET_CLIENT_SID]
    } else {
        &[]
    }
}

/// Paths the sandbox SID is granted for the run. Scratch is always there — it is the child's
/// only writable area — `--rw` is the explicit opt-in, and installer mode adds `$HOME`
/// (installers install), with the deny list subtracting back the interesting parts.
/// De-duplicated because `--rw` may repeat a path, or name the home / the scratch dir.
pub(crate) fn grant_paths(
    mode: Mode,
    scratch: &Path,
    rw: &[PathBuf],
    home: Option<&Path>,
) -> Vec<PathBuf> {
    let mut paths = vec![scratch.to_path_buf()];
    paths.extend(rw.iter().cloned());
    if mode == Mode::Installer {
        if let Some(h) = home {
            paths.push(h.to_path_buf());
        }
    }
    let mut seen: Vec<PathBuf> = Vec::with_capacity(paths.len());
    for p in paths {
        if !seen.contains(&p) {
            seen.push(p);
        }
    }
    seen
}

/// The shared deny list is written with `/` separators against a macOS home. Join it to a
/// Windows home with `\` and drop the entries that only exist there.
fn windows_rel(home: &Path, rel: &str) -> Option<PathBuf> {
    // `Library/...` covers the Keychain, the macOS browser profiles and LaunchAgents — no
    // Windows counterpart, and inventing one would hang a deny ACE off an unrelated path.
    if rel.starts_with("Library/") {
        return None;
    }
    let mut p = home.to_path_buf();
    p.push(rel.replace('/', "\\"));
    Some(p)
}

/// Windows-only additions to the shared secret list, relative to `$HOME`. The shared entries
/// cover the dotfile credential stores; these are the Windows locations the same material
/// lands in.
const WINDOWS_DENY_SECRET_EXTRA: &[&str] = &[
    // `gh` and cloud CLIs keep their tokens under %APPDATA% on Windows, not in the `~/.config`
    // tree the shared list points at.
    "AppData\\Roaming\\gh",
    "AppData\\Roaming\\Microsoft\\Credentials",
    "AppData\\Local\\Microsoft\\Credentials",
    // Browser profiles: cookies are DPAPI-encrypted to the user account; history and saved
    // logins are not.
    "AppData\\Local\\Google\\Chrome\\User Data",
    "AppData\\Local\\Microsoft\\Edge\\User Data",
    "AppData\\Roaming\\Mozilla\\Firefox\\Profiles",
];

/// Windows-only persistence hot spots, relative to `$HOME`: the per-user Startup folder is the
/// Windows equivalent of a LaunchAgent, and the Start Menu is the same idea one level up.
const WINDOWS_DENY_PERSIST_EXTRA: &[&str] = &[
    "AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs\\Startup",
    "AppData\\Roaming\\Microsoft\\Windows\\Start Menu",
];

/// Paths the installer-mode fence closes, mapped from the shared deny list plus the
/// Windows-specific additions. Paths that do not exist are still listed (the plan is a review
/// artifact); the runtime skips them.
pub(crate) fn deny_paths(home: &Path) -> Vec<(PathBuf, DenyKind)> {
    let mut out: Vec<(PathBuf, DenyKind)> = Vec::new();
    for (list, kind) in [
        (INSTALLER_DENY_READ, DenyKind::Secret),
        (WINDOWS_DENY_SECRET_EXTRA, DenyKind::Secret),
        (INSTALLER_DENY_WRITE, DenyKind::Persist),
        (WINDOWS_DENY_PERSIST_EXTRA, DenyKind::Persist),
    ] {
        for rel in list {
            if let Some(p) = windows_rel(home, rel) {
                out.push((p, kind));
            }
        }
    }
    out
}

/// The Windows counterpart of the seatbelt profile text: what the token gets, what it is denied,
/// and what it cannot reach at all. `--print-profile` prints this.
pub(crate) fn profile_plan(
    mode: Mode,
    allow_net: bool,
    scratch: &Path,
    rw: &[PathBuf],
    home: Option<&Path>,
) -> String {
    let caps = capabilities_for(mode, allow_net);
    let mut s =
        format!("backend: appcontainer\nprofile: {PROFILE_NAME}\ntoken: lowbox (no user SID)\n");
    s.push_str(&format!(
        "capabilities: {}\n",
        if caps.is_empty() {
            "none — no sockets at all".to_string()
        } else {
            caps.join(", ")
        }
    ));
    s.push_str(&format!("cwd: {}\n", scratch.display()));
    s.push_str("grant GENERIC_ALL (inherited by anything created inside):\n");
    for p in grant_paths(mode, scratch, rw, home) {
        s.push_str(&format!("  + {}\n", p.display()));
    }
    if mode == Mode::Installer {
        if let Some(h) = home {
            s.push_str("deny GENERIC_ALL:\n");
            for (p, kind) in deny_paths(h) {
                let label = match kind {
                    DenyKind::Secret => "secret",
                    DenyKind::Persist => "persist",
                };
                s.push_str(&format!("  - {label:<8}{}\n", p.display()));
            }
        }
    }
    s.push_str(
        "unreachable either way: everything else under the user profile (the lowbox carries no\n\
         user SID) — except %LOCALAPPDATA%\\Packages\\OpenCapX.Sandbox, the profile's own\n\
         package folder, which the lowbox can always write.\n",
    );
    s
}

#[cfg(target_os = "windows")]
mod imp {
    use super::*;
    use std::ffi::{c_void, OsStr, OsString};
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::sync::OnceLock;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, LocalFree, GENERIC_ALL, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSidToSidW, GetNamedSecurityInfoW, SetEntriesInAclW,
        SetNamedSecurityInfoW, DENY_ACCESS, EXPLICIT_ACCESS_W, GRANT_ACCESS, SE_FILE_OBJECT,
        TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
    };
    use windows_sys::Win32::Security::Isolation::{
        CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
    };
    use windows_sys::Win32::Security::{
        AclSizeInformation, EqualSid, GetAce, GetAclInformation, GetSecurityDescriptorDacl, ACL,
        ACL_SIZE_INFORMATION, CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, OBJECT_INHERIT_ACE,
        PSECURITY_DESCRIPTOR, PSID, SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES,
    };
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
        InitializeProcThreadAttributeList, ResumeThread, TerminateProcess,
        UpdateProcThreadAttribute, WaitForSingleObject, CREATE_SUSPENDED,
        EXTENDED_STARTUPINFO_PRESENT, INFINITE, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
        PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    };

    /// `HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS)` (0x800700B7) and its ERROR_FILE_EXISTS cousin
    /// (0x800700DF) — what `CreateAppContainerProfile` returns for a profile the registry
    /// already knows. The normal case after the first run, and **the SID out-slot stays null on
    /// this path** (nothing was created), so the null-SID check must not run before this
    /// acceptance: the CI run proved that ordering turns every run after the first into an
    /// "unavailable backend" — the fence only ever worked on first use.
    const HRESULT_ALREADY_EXISTS: i32 = -2147024713; // 0x800700B7
    const HRESULT_FILE_EXISTS: i32 = -2147024895; // 0x800700DF

    /// A SID this process owns outright: `ConvertStringSidToSidW` hands back a `LocalAlloc`
    /// block and `LocalFree` is its only correct release. Nothing else produces one — the
    /// profile SID is not owned, see [`profile_sid`].
    struct OwnedSid(PSID);

    impl Drop for OwnedSid {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: every SID that reaches here came from `ConvertStringSidToSidW`, so it
                // is a `LocalAlloc` block, and it is freed exactly once.
                unsafe { LocalFree(self.0.cast()) };
            }
        }
    }

    fn win_err(context: &str) -> std::io::Error {
        std::io::Error::other(format!("{context} (win32 error {})", unsafe {
            GetLastError()
        }))
    }

    fn wide(s: &str) -> Vec<u16> {
        OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn wide_path(p: &Path) -> Vec<u16> {
        p.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// The profile has to exist before its SID is usable as a grant trustee. Creating it is
    /// cheap: the first call registers it, every later one takes ERROR_ALREADY_EXISTS.
    ///
    /// Called only on the cold path: [`profile_sid`] tries
    /// `DeriveAppContainerSidFromAppContainerName` first and lands here only when the
    /// derivation says the profile is absent. The SID this call hands back is not ours to
    /// release (see the note on [`profile_sid`]), so it is left alone; at one leaked profile
    /// SID per process, on the first run on a machine and never again, that is the cheaper
    /// mistake than the heap corruption the alternative causes.
    fn ensure_profile() -> std::io::Result<()> {
        let name = wide(PROFILE_NAME);
        let display = wide("OpenCapX Sandbox");
        let description = wide("Isolation profile for opencapx sandbox runs");
        let mut sid: PSID = std::ptr::null_mut();
        // SAFETY: every input is a NUL-terminated buffer owned by this frame, `sid` is a valid
        // out-slot, and the capability array is null with a zero count.
        let hr = unsafe {
            CreateAppContainerProfile(
                name.as_ptr(),
                display.as_ptr(),
                description.as_ptr(),
                std::ptr::null(),
                0,
                &mut sid,
            )
        };
        if hr < 0 && hr != HRESULT_ALREADY_EXISTS && hr != HRESULT_FILE_EXISTS {
            return Err(std::io::Error::other(format!(
                "CreateAppContainerProfile failed (hr 0x{:08x})",
                hr as u32
            )));
        }
        Ok(())
    }

    fn sid_from_string(s: &str) -> Option<OwnedSid> {
        let w = wide(s);
        let mut raw: PSID = std::ptr::null_mut();
        // SAFETY: `w` is NUL-terminated, `raw` is a valid out-slot.
        if unsafe { ConvertStringSidToSidW(w.as_ptr(), &mut raw) } == 0 || raw.is_null() {
            return None;
        }
        Some(OwnedSid(raw))
    }

    /// The sandbox profile's SID: derived once per process, and never freed.
    ///
    /// The docs name `FreeSid` as the release for `DeriveAppContainerSidFromAppContainerName`
    /// and `CreateAppContainerProfile`, but a name that always resolves to the same profile
    /// does not get a private SID: userenv hands back a buffer it reuses across calls, so
    /// releasing it is a double free from the second call on. The Windows CI job showed
    /// exactly that shape — the whole suite ran in one process, and the first run in which
    /// every probe *succeeded* (and so derived and freed the same pointer again) died with a
    /// STATUS_ACCESS_VIOLATION inside the ACL write, which is the first thing after the free
    /// that dereferences the trustee. Six runs in a row failed that way, and neither
    /// `FreeSid` nor `LocalFree` moved it, because both are the process heap: the bug was
    /// the second release, not which one it used.
    ///
    /// Caching one pointer per process is the fix, and it is also the cheaper design: the
    /// profile SID is fixed for the machine's lifetime, so deriving it once is what the hot
    /// path wants anyway. A failure is *not* cached, so a machine that could not host the
    /// profile on the first call can still come back on a later one.
    fn profile_sid() -> std::io::Result<PSID> {
        static CACHED: OnceLock<usize> = OnceLock::new();
        if let Some(sid) = CACHED.get() {
            return Ok(*sid as PSID);
        }
        let name = wide(PROFILE_NAME);
        let mut raw: PSID = std::ptr::null_mut();
        // SAFETY: NUL-terminated name, valid out-slot.
        let mut hr = unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut raw) };
        if hr < 0 || raw.is_null() {
            // Cold path only: the derivation fails when the registry has no such profile.
            // Create it once, then derive again — every later run derives directly.
            ensure_profile()?;
            raw = std::ptr::null_mut();
            // SAFETY: as above.
            hr = unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut raw) };
        }
        if hr < 0 || raw.is_null() {
            return Err(std::io::Error::other(format!(
                "DeriveAppContainerSidFromAppContainerName failed (hr 0x{:08x})",
                hr as u32
            )));
        }
        // `set` loses the race against a second thread that got there first; both threads
        // derived the same profile, so the pointer is the same either way.
        let _ = CACHED.set(raw as usize);
        Ok(raw)
    }

    /// A second derivation that bypasses the cache, for the stability proof in the tests:
    /// without it, "the SID is stable across runs" would only be comparing the cache with
    /// itself. The returned buffer is left alive for the same reason [`profile_sid`] leaves
    /// its own — one uncached derivation per test process.
    #[cfg(test)]
    pub(crate) fn derive_profile_sid_uncached() -> std::io::Result<String> {
        let name = wide(PROFILE_NAME);
        let mut raw: PSID = std::ptr::null_mut();
        // SAFETY: NUL-terminated name, valid out-slot.
        let hr = unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut raw) };
        if hr < 0 || raw.is_null() {
            return Err(std::io::Error::other(format!(
                "DeriveAppContainerSidFromAppContainerName failed (hr 0x{:08x})",
                hr as u32
            )));
        }
        sid_to_string(raw)
    }
    fn sid_to_string(sid: PSID) -> std::io::Result<String> {
        let mut raw: *mut u16 = std::ptr::null_mut();
        // SAFETY: `sid` is a live SID, `raw` is a valid out-slot for a LocalAlloc'd string.
        if unsafe { ConvertSidToStringSidW(sid, &mut raw) } == 0 || raw.is_null() {
            return Err(win_err("ConvertSidToStringSidW"));
        }
        let mut len = 0usize;
        while unsafe { *raw.add(len) } != 0 {
            len += 1;
        }
        // SAFETY: `len` counted the NUL-terminated UTF-16 run starting at `raw`.
        let s = unsafe { OsString::from_wide(std::slice::from_raw_parts(raw, len)) };
        unsafe { LocalFree(raw.cast()) };
        Ok(s.to_string_lossy().into_owned())
    }

    /// One run's ACL state. `Drop` is the safety net: whatever happened to the child, every ACE
    /// this run added is taken back out before the caller moves on. An ACL left behind would
    /// hand the (stable, profile-wide) sandbox SID permanent access to a user directory.
    struct Grants {
        sid: PSID,
        /// Paths granted GENERIC_ALL.
        granted: Vec<PathBuf>,
        /// Paths denied GENERIC_ALL.
        denied: Vec<PathBuf>,
    }

    impl Grants {
        fn apply(
            mode: Mode,
            scratch: &Path,
            rw: &[PathBuf],
            home: Option<&Path>,
            sid: PSID,
        ) -> std::io::Result<Grants> {
            // Built before the first write, so a mid-way failure unwinds through `Drop`.
            let mut g = Grants {
                sid,
                granted: Vec::new(),
                denied: Vec::new(),
            };
            for p in grant_paths(mode, scratch, rw, home) {
                let current = read_dacl(&p)?;
                eprintln!("ocx:ac:ap:got-acl");
                let d = current.dacl();
                eprintln!("ocx:ac:ap:got-dacl {d:p}");
                write_dacl(&p, d, &allow_entry(g.sid))?;
                g.granted.push(p);
            }
            if mode == Mode::Installer {
                if let Some(h) = home {
                    for (p, _) in deny_paths(h) {
                        // A path that does not exist has nothing to protect, and there is no
                        // object to hang an ACE on.
                        if !p.exists() {
                            continue;
                        }
                        let current = read_dacl(&p)?;
                        write_dacl(&p, current.dacl(), &deny_entry(g.sid))?;
                        g.denied.push(p);
                    }
                }
            }
            Ok(g)
        }
    }

    impl Drop for Grants {
        fn drop(&mut self) {
            // Denies first: while the `$HOME` grant is still standing, removing the deny over
            // `~/.ssh` would leave a wide-open hole for the length of one ACL write.
            for p in self.denied.drain(..) {
                if let Err(e) = revoke(&p, &un_deny_entry(self.sid)) {
                    eprintln!(
                        "opencapx sandbox: WARNING: could not remove the deny ACE from {}: {e}",
                        p.display()
                    );
                }
            }
            for p in self.granted.drain(..) {
                if let Err(e) = revoke(&p, &un_allow_entry(self.sid)) {
                    eprintln!(
                        "opencapx sandbox: WARNING: could not remove the grant ACE from {}: {e}",
                        p.display()
                    );
                }
            }
        }
    }

    /// A path's current DACL. The ACL lives inside the returned descriptor, so both are kept
    /// together and freed together.
    struct PathAcl(PSECURITY_DESCRIPTOR);

    impl PathAcl {
        fn dacl(&self) -> *const ACL {
            let mut present = 0;
            let mut dacl: *mut ACL = std::ptr::null_mut();
            eprintln!("ocx:ac:gsd:begin psd={:p}", self.0);
            // SAFETY: `self.0` is a live self-relative security descriptor.
            if unsafe {
                GetSecurityDescriptorDacl(self.0, &mut present, &mut dacl, std::ptr::null_mut())
            } == 0
            {
                eprintln!("ocx:ac:gsd:rejected present={present} dacl={:p}", dacl);
                return std::ptr::null();
            }
            eprintln!("ocx:ac:gsd:done present={present} dacl={:p}", dacl);
            dacl
        }
    }

    impl Drop for PathAcl {
        fn drop(&mut self) {
            unsafe { LocalFree(self.0.cast()) };
        }
    }

    /// TEMP (PR #32 diagnosis): a breadcrumb per Win32 call — the CI ACCESS_VIOLATION happens
    /// somewhere inside the first guarded run and a crash names no API.
    fn read_dacl(path: &Path) -> std::io::Result<PathAcl> {
        eprintln!("ocx:ac:gsi:begin {}", path.display());
        let w = wide_path(path);
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: NUL-terminated path, valid out-slot for the descriptor; owner/group/SACL and
        // the DACL pointer are null because we only want the descriptor itself.
        let code = unsafe {
            GetNamedSecurityInfoW(
                w.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut sd,
            )
        };
        eprintln!("ocx:ac:gsi:done code={code} sd={:p}", sd);
        if code != 0 {
            return Err(std::io::Error::other(format!(
                "GetNamedSecurityInfoW failed ({code}) on {}",
                path.display()
            )));
        }
        Ok(PathAcl(sd))
    }

    fn write_dacl(
        path: &Path,
        current: *const ACL,
        entry: &EXPLICIT_ACCESS_W,
    ) -> std::io::Result<()> {
        let mut new_acl: *mut ACL = std::ptr::null_mut();
        // SAFETY: one valid entry; `current` is a live ACL or null (meaning "no DACL yet").
        eprintln!("ocx:ac:sea:begin current={current:p}");
        let code = unsafe { SetEntriesInAclW(1, entry, current, &mut new_acl) };
        eprintln!("ocx:ac:sea:done code={code} acl={:p}", new_acl);
        if code != 0 {
            return Err(std::io::Error::other(format!(
                "SetEntriesInAclW failed ({code}) for {}",
                path.display()
            )));
        }
        let w = wide_path(path);
        eprintln!("ocx:ac:sni:begin");
        // SAFETY: NUL-terminated path; owner/group/SACL null (leave them alone); `new_acl` was
        // allocated for us and is freed right after, including on the error path.
        let code = unsafe {
            SetNamedSecurityInfoW(
                w.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                new_acl,
                std::ptr::null_mut(),
            )
        };
        eprintln!("ocx:ac:sni:done code={code}");
        unsafe { LocalFree(new_acl.cast()) };
        if code != 0 {
            return Err(std::io::Error::other(format!(
                "SetNamedSecurityInfoW failed ({code}) on {}",
                path.display()
            )));
        }
        Ok(())
    }

    fn revoke(path: &Path, entry: &EXPLICIT_ACCESS_W) -> std::io::Result<()> {
        let current = read_dacl(path)?;
        write_dacl(path, current.dacl(), entry)
    }

    fn trustee(sid: PSID) -> TRUSTEE_W {
        TRUSTEE_W {
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: sid as *mut u16,
            ..Default::default()
        }
    }

    fn inherit() -> u32 {
        CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE
    }

    /// Inheritance is what makes the fence usable: a command that creates a file inside a
    /// granted directory needs no second grant.
    fn allow_entry(sid: PSID) -> EXPLICIT_ACCESS_W {
        EXPLICIT_ACCESS_W {
            grfAccessPermissions: GENERIC_ALL,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: inherit(),
            Trustee: trustee(sid),
        }
    }

    /// A lowbox grant is all-or-nothing, so the installer deny list is enforced as a full
    /// `GENERIC_ALL` deny: it has to out-rank the `$HOME` grant installer mode installs.
    fn deny_entry(sid: PSID) -> EXPLICIT_ACCESS_W {
        EXPLICIT_ACCESS_W {
            grfAccessPermissions: GENERIC_ALL,
            grfAccessMode: DENY_ACCESS,
            grfInheritance: inherit(),
            Trustee: trustee(sid),
        }
    }

    /// `SetEntriesInAclW`'s removal idiom: a zero-permission entry for the same trustee strips
    /// the matching ACEs out of the DACL handed to it as `old_acl`. The access mode is the
    /// *opposite* of the mode being removed.
    fn un_allow_entry(sid: PSID) -> EXPLICIT_ACCESS_W {
        EXPLICIT_ACCESS_W {
            grfAccessPermissions: 0,
            grfAccessMode: DENY_ACCESS,
            grfInheritance: inherit(),
            Trustee: trustee(sid),
        }
    }

    fn un_deny_entry(sid: PSID) -> EXPLICIT_ACCESS_W {
        EXPLICIT_ACCESS_W {
            grfAccessPermissions: 0,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: inherit(),
            Trustee: trustee(sid),
        }
    }

    /// True when `sid_string` appears in the DACL of `path`. This is what makes ACL cleanup
    /// testable: after a run, a `--rw` directory must not carry the sandbox SID.
    pub(crate) fn acl_has_sid(path: &Path, sid_string: &str) -> bool {
        let Some(sid) = sid_from_string(sid_string) else {
            return false;
        };
        let Ok(current) = read_dacl(path) else {
            return false;
        };
        let dacl = current.dacl();
        if dacl.is_null() {
            return false;
        }
        let mut info: ACL_SIZE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a correctly sized out-buffer for AclSizeInformation.
        if unsafe {
            GetAclInformation(
                dacl,
                &mut info as *mut ACL_SIZE_INFORMATION as *mut c_void,
                size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
        } == 0
        {
            return false;
        }
        for i in 0..info.AceCount {
            let mut ace: *mut c_void = std::ptr::null_mut();
            // SAFETY: `i` is below AceCount, `ace` is a valid out-slot.
            if unsafe { GetAce(dacl, i, &mut ace) } == 0 || ace.is_null() {
                continue;
            }
            // SAFETY: every ACE starts with the 4-byte header, whose u16 AceSize sits at
            // offset 2; anything shorter than 12 bytes cannot hold a full SID after the mask.
            let ace_size = unsafe { (ace as *const u8).add(2).cast::<u16>().read_unaligned() };
            if (ace_size as usize) < 12 {
                continue;
            }
            // Only the plain ACCESS_ALLOWED_ACE / ACCESS_DENIED_ACE forms are ever written here
            // (SetEntriesInAclW emits them for a non-GUID trustee); those put the SID right after
            // the header and the 4-byte access mask.
            // SAFETY: AceSize >= 12 means 8 header/mask bytes plus a complete SID.
            let ace_sid = unsafe { (ace as *const u8).add(8).cast::<c_void>() };
            // SAFETY: both pointers are live SIDs.
            if unsafe { EqualSid(ace_sid.cast_mut(), sid.0) } != 0 {
                return true;
            }
        }
        false
    }

    /// Probe: the profile can be created/derived and its SID is well-formed. Cheap enough for
    /// every `--check` and every run, and it fails with a reason instead of erroring deep inside
    /// `CreateProcessW`.
    pub(crate) fn probe() -> std::io::Result<()> {
        sid_to_string(profile_sid()?)?;
        Ok(())
    }

    /// The sandbox profile SID in string form, for the ACL tests.
    #[cfg(test)]
    pub(crate) fn profile_sid_string() -> std::io::Result<String> {
        sid_to_string(profile_sid()?)
    }

    pub(crate) fn run(parsed: &Parsed, scratch: &Path) -> std::io::Result<i32> {
        let home = crate::core::home_dir().map(|h| canonicalize_lossy(&h));
        let sid = profile_sid()?;
        let grants = Grants::apply(
            parsed.profile,
            scratch,
            &parsed.policy.rw,
            home.as_deref(),
            sid,
        )?;
        let code = spawn_and_wait(
            parsed,
            grants.sid,
            capabilities_for(parsed.profile, parsed.policy.allow_net),
            scratch,
        );
        // Whether the child ran, failed to start, or timed out — the grants go back now.
        drop(grants);
        code
    }

    /// Windows wants a double-NUL-terminated block of `KEY=VALUE` pairs in UTF-16 sorted by
    /// name, not a `Command`, so the shared env policy is applied to a variable list instead
    /// ([`filter_env`]). `TEMP`/`TMP` are forced into the scratch dir: the lowbox cannot write
    /// the user's `%TEMP%`, and a tool with nowhere to put its temp files is broken — the same
    /// reason the seatbelt profile grants `TMPDIR`.
    fn env_block(policy: EnvPolicy, scratch: &Path) -> Vec<u16> {
        let mut entries: Vec<(String, OsString)> = filter_env(policy)
            .into_iter()
            .map(|(k, v)| (k.to_string_lossy().to_uppercase(), v))
            .collect();
        for key in ["TEMP", "TMP"] {
            let value = scratch.as_os_str().to_owned();
            match entries.iter_mut().find(|(k, _)| k == key) {
                Some(slot) => slot.1 = value,
                None => entries.push((key.to_string(), value)),
            }
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let mut block: Vec<u16> = Vec::new();
        for (k, v) in entries {
            block.extend(k.encode_utf16());
            block.push(u16::from(b'='));
            block.extend(v.encode_wide());
            block.push(0);
        }
        block.push(0);
        block
    }

    /// The CRT quoting rules, which is what every Windows command-line parser implements: quote
    /// when the argument is empty or holds whitespace or a quote, and double any backslash run
    /// that precedes a quote or ends the argument.
    fn quote_arg(arg: &str) -> String {
        if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\u{1b}', '"']) {
            return arg.to_string();
        }
        let mut out = String::from("\"");
        let mut slashes = 0usize;
        for c in arg.chars() {
            if c == '\\' {
                slashes += 1;
                continue;
            }
            if c == '"' {
                out.push_str(&"\\".repeat(slashes * 2 + 1));
                slashes = 0;
                out.push('"');
                continue;
            }
            out.push_str(&"\\".repeat(slashes));
            slashes = 0;
            out.push(c);
        }
        out.push_str(&"\\".repeat(slashes * 2));
        out.push('"');
        out
    }

    fn command_line(command: &[String]) -> Vec<u16> {
        let mut line: Vec<u16> = Vec::new();
        for (i, arg) in command.iter().enumerate() {
            if i > 0 {
                line.push(u16::from(b' '));
            }
            line.extend(quote_arg(arg).encode_utf16());
        }
        line.push(0);
        line
    }

    fn exit_code(process: windows_sys::Win32::Foundation::HANDLE) -> i32 {
        let mut code = 0u32;
        // SAFETY: live process handle, valid out-slot.
        if unsafe { GetExitCodeProcess(process, &mut code) } == 0 {
            return 1;
        }
        code as i32
    }

    fn abort_before_start(pi: &PROCESS_INFORMATION, job: windows_sys::Win32::Foundation::HANDLE) {
        // The child is still suspended, so terminating it is a clean no-op with no side effects.
        unsafe { TerminateProcess(pi.hProcess, 1) };
        unsafe { CloseHandle(pi.hThread) };
        unsafe { CloseHandle(pi.hProcess) };
        unsafe { CloseHandle(job) };
    }

    /// A Job Object is the Windows answer to the process-group kill: grandchildren join the job
    /// when they are created, so `TerminateJobObject` sweeps the whole tree in one call.
    /// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` is deliberately *not* set — it would also kill a
    /// service an installer legitimately left running, which the Unix path explicitly allows.
    fn spawn_and_wait(
        parsed: &Parsed,
        sid: PSID,
        caps: &[&str],
        scratch: &Path,
    ) -> std::io::Result<i32> {
        // Capabilities are SIDs, not GUIDs (`S-1-15-3-1` = internetClient). The array has to
        // outlive the UpdateProcThreadAttribute call, so it lives to the end of this function.
        let mut cap_owners: Vec<OwnedSid> = Vec::new();
        let mut cap_sids: Vec<SID_AND_ATTRIBUTES> = Vec::new();
        for text in caps {
            if let Some(s) = sid_from_string(text) {
                cap_sids.push(SID_AND_ATTRIBUTES {
                    Sid: s.0,
                    Attributes: 0,
                });
                cap_owners.push(s);
            }
        }
        let security_caps = SECURITY_CAPABILITIES {
            AppContainerSid: sid,
            Capabilities: if cap_sids.is_empty() {
                std::ptr::null_mut()
            } else {
                cap_sids.as_mut_ptr()
            },
            CapabilityCount: cap_sids.len() as u32,
            Reserved: 0,
        };

        // Two-step attribute-list init: the first call fails with ERROR_INSUFFICIENT_BUFFER and
        // reports the size the second one needs.
        let mut attr_bytes: usize = 0;
        // SAFETY: a null list is the documented size query; `attr_bytes` is a valid out-slot.
        unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut attr_bytes) };
        let mut attr_buf = vec![0u8; attr_bytes];
        let attr_list = attr_buf.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        // SAFETY: `attr_buf` is at least the size just reported, for the one attribute below.
        if unsafe { InitializeProcThreadAttributeList(attr_list, 1, 0, &mut attr_bytes) } == 0 {
            return Err(win_err("InitializeProcThreadAttributeList"));
        }
        // SAFETY: the list is initialized for one attribute and `security_caps` outlives this.
        if unsafe {
            UpdateProcThreadAttribute(
                attr_list,
                0,
                PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                &security_caps as *const SECURITY_CAPABILITIES as *const c_void,
                size_of::<SECURITY_CAPABILITIES>() as usize,
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        } == 0
        {
            unsafe { DeleteProcThreadAttributeList(attr_list) };
            return Err(win_err("UpdateProcThreadAttribute"));
        }

        let mut si = STARTUPINFOEXW::default();
        si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        // SAFETY: each is the one-argument std-handle getter.
        let stdin = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        let stdout = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        let stderr = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
        // STARTF_USESTDHANDLES with a dead handle hands the child a broken stdio and it can die
        // on it. GetStdHandle signals "no console" (a service, a detached test run) with
        // INVALID_HANDLE_VALUE as often as with null — both count as absent here, and the
        // flags stay clear so the child picks up the session's own console.
        let dead = |h: windows_sys::Win32::Foundation::HANDLE| {
            h.is_null() || h as usize == std::usize::MAX
        };
        if !dead(stdin) && !dead(stdout) && !dead(stderr) {
            si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            si.StartupInfo.hStdInput = stdin;
            si.StartupInfo.hStdOutput = stdout;
            si.StartupInfo.hStdError = stderr;
        }
        si.lpAttributeList = attr_list;

        // Everything the kernel reads out of these has to stay alive across CreateProcessW.
        let mut cmdline = command_line(&parsed.command);
        let env = env_block(parsed.policy.env, scratch);
        let cwd = wide_path(scratch);
        let mut pi = PROCESS_INFORMATION::default();
        // SAFETY: mutable NUL-terminated command line, inherited stdio handles, a live
        // environment block, a live cwd, a STARTUPINFOEXW whose first field is the
        // STARTUPINFOW CreateProcessW reads, and a valid PROCESS_INFORMATION out-slot.
        let started = unsafe {
            CreateProcessW(
                std::ptr::null(),
                cmdline.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED,
                env.as_ptr() as *const c_void,
                cwd.as_ptr(),
                &si.StartupInfo,
                &mut pi,
            )
        };
        // The list and the buffer behind it are ours again the moment CreateProcessW returns.
        unsafe { DeleteProcThreadAttributeList(attr_list) };
        if started == 0 {
            return Err(win_err("CreateProcessW"));
        }

        // From here the child exists, so nothing may return Err — the caller treats that as
        // "backend failed to start" and would run the command again, unguarded.
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            let err = win_err("CreateJobObjectW");
            abort_before_start(&pi, job);
            return Err(err);
        }
        // Started suspended, so the child cannot spawn a grandchild that escapes the job between
        // CreateProcessW and the assign.
        // SAFETY: both handles are live and freshly created.
        if unsafe { AssignProcessToJobObject(job, pi.hProcess) } == 0 {
            let err = win_err("AssignProcessToJobObject");
            abort_before_start(&pi, job);
            return Err(err);
        }
        // SAFETY: the primary thread was created suspended and holds a suspension count of 1.
        if unsafe { ResumeThread(pi.hThread) } == u32::MAX {
            let err = win_err("ResumeThread");
            abort_before_start(&pi, job);
            return Err(err);
        }

        let code = match parsed.timeout_secs {
            Some(secs) => {
                let millis = secs.saturating_mul(1000).min(u32::MAX as u64) as u32;
                // SAFETY: live process handle.
                match unsafe { WaitForSingleObject(pi.hProcess, millis) } {
                    WAIT_TIMEOUT => {
                        unsafe { TerminateJobObject(job, 124) };
                        unsafe { WaitForSingleObject(pi.hProcess, 5000) };
                        eprintln!(
                            "opencapx sandbox: timeout after {secs}s; killed the AppContainer job"
                        );
                        124
                    }
                    // A wait failure still leaves the state unknown; the child owns its job and
                    // we report whatever it last had.
                    _ => exit_code(pi.hProcess),
                }
            }
            None => {
                unsafe { WaitForSingleObject(pi.hProcess, INFINITE) };
                exit_code(pi.hProcess)
            }
        };
        unsafe { CloseHandle(pi.hThread) };
        unsafe { CloseHandle(pi.hProcess) };
        unsafe { CloseHandle(job) };
        Ok(code)
    }
}

#[cfg(target_os = "windows")]
pub(crate) use imp::{acl_has_sid, probe as probe_backend, run as run_appcontainer};
#[cfg(all(target_os = "windows", test))]
pub(crate) use imp::{derive_profile_sid_uncached, profile_sid_string};

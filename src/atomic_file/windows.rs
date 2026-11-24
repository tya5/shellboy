use std::{
    fs::File,
    io,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::Path,
    ptr::null_mut,
};
use windows_sys::Win32::{
    Foundation::{ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED, INVALID_HANDLE_VALUE, LocalFree},
    Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER,
        Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SE_FILE_OBJECT,
        },
        DACL_SECURITY_INFORMATION, EqualSid, GetAce, INHERIT_ONLY_ACE, IsWellKnownSid,
        OWNER_SECURITY_INFORMATION, SECURITY_ATTRIBUTES, WinBuiltinAdministratorsSid,
        WinCreatorOwnerRightsSid, WinLocalSystemSid,
    },
    Storage::FileSystem::{
        CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_WRITE, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    },
};

struct Descriptor(windows_sys::Win32::Security::PSECURITY_DESCRIPTOR);
impl Drop for Descriptor {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // Both APIs allocate this descriptor with LocalAlloc.
            unsafe { LocalFree(self.0) };
        }
    }
}

pub(super) fn private_create(path: &Path) -> io::Result<File> {
    // Protected DACL: the owner (including future ownership changes) and LocalSystem.
    let sddl: Vec<u16> = "D:P(A;;FA;;;OW)(A;;FA;;;SY)\0".encode_utf16().collect();
    let mut descriptor = Descriptor(null_mut());
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor.0,
            null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let security = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains NUL",
        ));
    }
    wide.push(0);
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            &security,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        )
    };
    if handle != INVALID_HANDLE_VALUE {
        // The handle is fresh and non-inheritable; File now owns it.
        return Ok(unsafe { File::from_raw_handle(handle) });
    }
    let error = io::Error::last_os_error();
    if error
        .raw_os_error()
        .is_some_and(|e| e == ERROR_NOT_SUPPORTED as i32 || e == ERROR_INVALID_FUNCTION as i32)
    {
        // Filesystems without ACLs remain usable; the caller reports unverifiable privacy.
        return File::options().write(true).create_new(true).open(path);
    }
    Err(error)
}

pub(super) fn warning(path: &Path) -> Option<&'static str> {
    match private_acl(path) {
        Ok(true) => None,
        Ok(false) => Some(
            "File permissions may grant access beyond its owner; use a private location or tighten its Windows ACL",
        ),
        Err(_) => {
            Some("Could not verify private Windows file permissions; check the destination ACL")
        }
    }
}

fn private_acl(path: &Path) -> io::Result<bool> {
    let file = File::open(path)?;
    let mut descriptor = Descriptor(null_mut());
    let (mut owner, mut dacl) = (null_mut(), null_mut());
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor.0,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    if dacl.is_null() || owner.is_null() {
        return Ok(false);
    }
    // A successful GetSecurityInfo supplies pointers into the live descriptor.
    // This is conservative: complicated ACEs are not certified as private.
    for i in 0..u32::from(unsafe { (*dacl).AceCount }) {
        let mut ace = null_mut();
        if unsafe { GetAce(dacl, i, &mut ace) } == 0 || ace.is_null() {
            return Err(io::Error::last_os_error());
        }
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        if u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0 || header.AceType == 1 {
            continue;
        }
        if header.AceType != 0 || usize::from(header.AceSize) < size_of::<ACCESS_ALLOWED_ACE>() {
            return Ok(false);
        }
        let sid =
            unsafe { std::ptr::addr_of_mut!((*ace.cast::<ACCESS_ALLOWED_ACE>()).SidStart).cast() };
        let trusted = unsafe {
            EqualSid(owner, sid) != 0
                || IsWellKnownSid(sid, WinCreatorOwnerRightsSid) != 0
                || IsWellKnownSid(sid, WinLocalSystemSid) != 0
                || IsWellKnownSid(sid, WinBuiltinAdministratorsSid) != 0
        };
        if !trusted {
            return Ok(false);
        }
    }
    Ok(true)
}

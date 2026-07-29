#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallChannel {
    Portable,
    Winget,
}

#[derive(Clone, Debug)]
pub struct ReleaseDescriptor {
    pub latest_version: String,
}

pub fn current_install_channel() -> InstallChannel {
    InstallChannel::Portable
}

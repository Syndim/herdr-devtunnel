mod doctor;
mod setup;

#[cfg(windows)]
pub(crate) use doctor::check_via_contract;
pub(crate) use doctor::doctor;
pub(crate) use setup::setup;

//! Compile-time application and dependency identity.

pub const APP_ID: &str = "com.echo.chat";
pub const VERSION_LABEL: &str = concat!("v", env!("CARGO_PKG_VERSION"));
pub const USER_AGENT: &str = concat!(
    "Echo/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/Revulate/Echo)"
);
pub const VERSION_REPORT: &str = concat!(
    "Echo ",
    env!("CARGO_PKG_VERSION"),
    "\nGPUI crate ",
    env!("ECHO_GPUI_VERSION"),
    "\nGPUI source Zed ",
    env!("ECHO_GPUI_ZED_REV"),
    "\nTarget ",
    env!("ECHO_BUILD_TARGET")
);

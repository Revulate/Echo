"use strict";

// wayland_native_driver.py copies this template into its isolated run root and
// replaces the closed nonce placeholder. It is loaded directly through KWin's
// DBus scripting API and never installed into user configuration.
const campaignNonce = "__ECHO_CAMPAIGN_NONCE__";
const expectedPid = __ECHO_EXPECTED_PID__;

function geometry(value) {
    return {
        x: Number(value.x),
        y: Number(value.y),
        width: Number(value.width),
        height: Number(value.height),
    };
}

function publishActiveWindow() {
    const window = workspace.activeWindow;
    if (!window || !window.output || Number(window.pid) !== expectedPid) {
        return;
    }
    const windowReceipt = {
        pid: Number(window.pid),
        internal_id: String(window.internalId),
        geometry: geometry(window.frameGeometry),
        output: String(window.output.name),
        focused: workspace.activeWindow === window,
        active: Boolean(window.active),
        app_id: String(window.desktopFileName || ""),
        resource_class: String(window.resourceClass || ""),
        title: String(window.caption || ""),
    };
    const displayReceipt = {
        output: String(window.output.name),
        geometry: geometry(window.output.geometry),
        scale: Number(window.output.devicePixelRatio),
    };
    callDBus(
        "org.echo.Benchmark",
        "/org/echo/Benchmark",
        "org.echo.Benchmark",
        "PublishWindow",
        campaignNonce,
        JSON.stringify(windowReceipt),
        JSON.stringify(displayReceipt),
        function (accepted) {
            print("echo-benchmark-receipt: publish accepted=" + String(accepted));
        }
    );
}

publishActiveWindow();
workspace.windowActivated.connect(publishActiveWindow);

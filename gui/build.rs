fn main() {
    // `#[cfg(windows)]` reflects the *host* compiling this build script, not
    // the `--target` of the binary being built. That's exactly what we want
    // here: the release pipeline builds Windows binaries natively on a
    // windows-latest CI runner (host == target == Windows), so this embeds
    // the icon there. It's a no-op when cross-compiling a Windows target
    // from a non-Windows host, which this project's CI doesn't do.
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        if let Err(e) = res.compile() {
            eprintln!("warning: failed to embed Windows icon resource: {e}");
        }
    }
}

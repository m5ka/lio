//! Asks the user whether to trust a certificate.

use adw::prelude::*;

/// Shows the fingerprint and returns whether the user chose to trust it.
pub async fn ask_trust(parent: &impl IsA<gtk::Widget>, fingerprint_sha256: &str) -> bool {
    let builder = gtk::Builder::from_resource("/dev/m5ka/Lio/ui/cert_dialog.ui");
    let dialog: adw::AlertDialog = builder.object("dialog").expect("dialog in cert_dialog.ui");
    let label: gtk::Label = builder
        .object("fingerprint_label")
        .expect("fingerprint label");
    label.set_label(&format_fingerprint(fingerprint_sha256));
    dialog.choose_future(Some(parent)).await == "trust"
}

/// `ab:cd:ef:…` in two lines, as browsers show it.
fn format_fingerprint(hex: &str) -> String {
    let pairs: Vec<&str> = hex
        .as_bytes()
        .chunks(2)
        .map(|c| std::str::from_utf8(c).unwrap_or(""))
        .collect();
    let half = pairs.len().div_ceil(2);
    let upper = |p: &[&str]| p.join(":").to_uppercase();
    format!(
        "SHA-256\n{}\n{}",
        upper(&pairs[..half]),
        upper(&pairs[half..])
    )
}

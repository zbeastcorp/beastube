//! Build script: generates the Tauri context, embeds the capability schemas and, on Windows, the
//! application manifest and icon resources.

fn main() {
    tauri_build::build();
}

//! Upload, list, stream and delete files in a storage zone.
//!
//! ```sh
//! BUNNY_STORAGE_ZONE=my-zone BUNNY_STORAGE_PASSWORD=... BUNNY_STORAGE_REGION=ny \
//!   cargo run --example storage -- path/to/local/file
//! ```

use std::time::Duration;

use bunnysdk::Config;
use bunnysdk::storage::{Region, StorageClient, UploadOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let zone = std::env::var("BUNNY_STORAGE_ZONE")?;
    let password = std::env::var("BUNNY_STORAGE_PASSWORD")?;
    let region: Region = std::env::var("BUNNY_STORAGE_REGION")
        .unwrap_or_else(|_| "de".into())
        .parse()?;
    let local = std::env::args().nth(1).ok_or("usage: storage <file>")?;

    let config = Config::new()
        .timeout(Duration::from_secs(60))
        .max_retries(3);
    let storage = StorageClient::with_config(zone, &password, region, config)?;

    // Small upload from memory, with an automatic checksum.
    storage.upload("example/hello.txt", "hello").await?;

    // Large upload streamed from disk.
    let file = tokio::fs::File::open(&local).await?;
    storage
        .upload_with(
            "example/upload.bin",
            bunnysdk::reqwest::Body::from(file),
            UploadOptions::new().content_type("application/octet-stream"),
        )
        .await?;

    for object in storage.list("example").await? {
        println!("{} ({} bytes)", object.object_name, object.length);
    }

    // Streamed download.
    let mut download = storage.download_stream("example/upload.bin").await?;
    let mut total = 0;
    while let Some(chunk) = download.chunk().await? {
        total += chunk.len();
    }
    println!("downloaded {total} bytes");

    match storage.download("example/missing.txt").await {
        Err(e) if e.is_not_found() => println!("missing.txt: not found, as expected"),
        other => println!("unexpected: {other:?}"),
    }

    storage.delete("example/").await?;
    Ok(())
}

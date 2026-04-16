use axum::{Json, extract::Multipart, http::StatusCode};
use uuid::Uuid;

use crate::auth::AuthUser;

const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif", "bmp"];
const MAX_IMAGE_DIMENSION: u32 = 1920;
const COMPRESS_THRESHOLD: usize = 512_000; // skip re-encoding for images under 512KB that fit in dimensions

#[derive(serde::Serialize)]
pub struct UploadResponse {
    pub url: String,
}

pub async fn upload(
    AuthUser(_, _): AuthUser,
    mut multipart: Multipart,
) -> Result<Json<UploadResponse>, StatusCode> {
    tracing::info!("upload: handler entered");

    let field = match multipart.next_field().await {
        Ok(Some(f)) => f,
        Ok(None) => {
            tracing::error!("upload: no field in multipart");
            return Err(StatusCode::BAD_REQUEST);
        }
        Err(e) => {
            tracing::error!("upload: failed to read multipart field: {e}");
            return Err(StatusCode::BAD_REQUEST);
        }
    };

    let original_name = field.file_name().unwrap_or("file").to_string();
    let ext = original_name
        .rsplit('.')
        .next()
        .unwrap_or("bin")
        .to_lowercase();

    tracing::info!("upload: file={original_name} ext={ext}");

    let data = match field.bytes().await {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("upload: failed to read bytes: {e}");
            return Err(StatusCode::BAD_REQUEST);
        }
    };

    tracing::info!("upload: received {} bytes", data.len());

    if data.len() > 20 * 1024 * 1024 {
        tracing::warn!("upload: file too large ({} bytes)", data.len());
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }

    let is_image = IMAGE_EXTS.contains(&ext.as_str());

    if is_image {
        let data_vec = data.to_vec();
        let compressed = match tokio::task::spawn_blocking(move || compress_image(&data_vec)).await
        {
            Ok(Some(bytes)) => {
                tracing::info!("upload: compressed to {} bytes", bytes.len());
                Some(bytes)
            }
            Ok(None) => {
                tracing::info!("upload: compression skipped (small file within dimensions)");
                None
            }
            Err(e) => {
                tracing::warn!("upload: spawn_blocking failed: {e}");
                None
            }
        };

        if let Some(bytes) = compressed {
            let filename = format!("{}.jpg", Uuid::new_v4());
            let path = format!("static/uploads/{}", filename);
            if let Err(e) = tokio::fs::write(&path, &bytes).await {
                tracing::error!("upload: failed to write compressed file {path}: {e}");
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
            tracing::info!("upload: saved compressed as {filename}");
            return Ok(Json(UploadResponse {
                url: format!("/uploads/{}", filename),
            }));
        }
    }

    let filename = format!("{}.{}", Uuid::new_v4(), ext);
    let path = format!("static/uploads/{}", filename);
    if let Err(e) = tokio::fs::write(&path, &data).await {
        tracing::error!("upload: failed to write file {path}: {e}");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    tracing::info!("upload: saved as {filename}");

    Ok(Json(UploadResponse {
        url: format!("/uploads/{}", filename),
    }))
}

/// Compress and resize an image. Returns None if the image is small enough to skip.
fn compress_image(data: &[u8]) -> Option<Vec<u8>> {
    let img = image::load_from_memory(data).ok()?;

    let needs_resize = img.width() > MAX_IMAGE_DIMENSION || img.height() > MAX_IMAGE_DIMENSION;

    // Skip compression for small files that don't need resizing
    if !needs_resize && data.len() < COMPRESS_THRESHOLD {
        return None;
    }

    let img = if needs_resize {
        img.resize(
            MAX_IMAGE_DIMENSION,
            MAX_IMAGE_DIMENSION,
            image::imageops::FilterType::Triangle, // much faster than Lanczos3, good enough for web
        )
    } else {
        img
    };
    let rgb = img.to_rgb8();
    let mut buf = std::io::Cursor::new(Vec::new());
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 80);
    image::ImageEncoder::write_image(
        encoder,
        &rgb,
        rgb.width(),
        rgb.height(),
        image::ExtendedColorType::Rgb8,
    )
    .ok()?;
    Some(buf.into_inner())
}

/// Delete an uploaded file given its URL path (e.g. "/uploads/abc.jpg")
pub fn delete_upload(url: &str) {
    if let Some(filename) = url.strip_prefix("/uploads/")
        && !filename.contains('/') && !filename.contains("..") {
            let path = format!("static/uploads/{}", filename);
            tokio::spawn(async move {
                let _ = tokio::fs::remove_file(&path).await;
            });
        }
}

use axum::{Json, extract::Multipart, http::StatusCode};
use uuid::Uuid;

const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif", "bmp"];
const MAX_IMAGE_DIMENSION: u32 = 1920;

#[derive(serde::Serialize)]
pub struct UploadResponse {
    pub url: String,
}

pub async fn upload(
    mut multipart: Multipart,
) -> Result<Json<UploadResponse>, StatusCode> {
    let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?
    else {
        return Err(StatusCode::BAD_REQUEST);
    };

    let original_name = field.file_name().unwrap_or("file").to_string();
    let ext = original_name
        .rsplit('.')
        .next()
        .unwrap_or("bin")
        .to_lowercase();

    let data = field
        .bytes()
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    if data.len() > 20 * 1024 * 1024 {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }

    let is_image = IMAGE_EXTS.contains(&ext.as_str());

    if is_image {
        // Compress image: resize if too large, save as JPEG
        let data_vec = data.to_vec();
        let result = tokio::task::spawn_blocking(move || compress_image(&data_vec))
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .map_err(|_| StatusCode::BAD_REQUEST)?;

        let filename = format!("{}.jpg", Uuid::new_v4());
        let path = format!("static/uploads/{}", filename);
        tokio::fs::write(&path, &result)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        Ok(Json(UploadResponse {
            url: format!("/uploads/{}", filename),
        }))
    } else {
        // Audio or other file — save as-is
        let filename = format!("{}.{}", Uuid::new_v4(), ext);
        let path = format!("static/uploads/{}", filename);
        tokio::fs::write(&path, &data)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        Ok(Json(UploadResponse {
            url: format!("/uploads/{}", filename),
        }))
    }
}

fn compress_image(data: &[u8]) -> Result<Vec<u8>, image::ImageError> {
    let img = image::load_from_memory(data)?;

    // Resize if either dimension exceeds the max
    let img = if img.width() > MAX_IMAGE_DIMENSION || img.height() > MAX_IMAGE_DIMENSION {
        img.resize(
            MAX_IMAGE_DIMENSION,
            MAX_IMAGE_DIMENSION,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        img
    };

    // Encode as JPEG at 80% quality
    let mut buf = std::io::Cursor::new(Vec::new());
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 80);
    img.write_with_encoder(encoder)?;
    Ok(buf.into_inner())
}

/// Delete an uploaded file given its URL path (e.g. "/uploads/abc.jpg")
pub fn delete_upload(url: &str) {
    if let Some(filename) = url.strip_prefix("/uploads/") {
        // Sanitize: only allow simple filenames (no path traversal)
        if !filename.contains('/') && !filename.contains("..") {
            let path = format!("static/uploads/{}", filename);
            tokio::spawn(async move {
                let _ = tokio::fs::remove_file(&path).await;
            });
        }
    }
}

//! 图片输入处理：魔数嗅探 MIME、构造 data URI、归一化 API 传入的图片参数。
//! 图片只在当轮以 OpenAI vision 的 image_url 段传给模型，不落库。

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;

/// 按文件魔数识别常见图片格式；识别不了按 JPEG 处理（上游通常也能容错）。
pub fn sniff_mime(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\xFF\xD8\xFF") {
        "image/jpeg"
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(b"GIF8") {
        "image/gif"
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else if bytes.starts_with(b"BM") {
        "image/bmp"
    } else {
        "image/jpeg"
    }
}

pub fn to_data_uri(bytes: &[u8]) -> String {
    format!("data:{};base64,{}", sniff_mime(bytes), BASE64.encode(bytes))
}

/// 归一化 API 请求里的一项图片：
/// - `http(s)://…` 原样透传（由上游 AI 提供商拉取）；
/// - `data:…;base64,…` 或裸 base64：解码校验、限制大小，再按嗅探出的 MIME 重建 data URI。
pub fn normalize_input(raw: &str, max_bytes: usize) -> Result<String> {
    let value = raw.trim();
    if value.is_empty() {
        bail!("images 里包含空项");
    }
    if value.starts_with("http://") || value.starts_with("https://") {
        return Ok(value.to_string());
    }
    let payload = match value.strip_prefix("data:") {
        Some(rest) => {
            let (meta, data) = rest.split_once(',').context("data URI 缺少逗号分隔")?;
            if !meta.ends_with(";base64") {
                bail!("data URI 必须是 base64 编码");
            }
            data
        }
        None => value,
    };
    // 宽容常见的换行/空白（有些客户端会给 base64 分行）。
    let compact: String = payload.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = BASE64
        .decode(compact.as_bytes())
        .context("图片不是合法的 base64")?;
    if bytes.is_empty() {
        bail!("图片内容为空");
    }
    if bytes.len() > max_bytes {
        bail!("图片过大：{} 字节，上限 {max_bytes} 字节", bytes.len());
    }
    Ok(to_data_uri(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_HEAD: &[u8] = b"\x89PNG\r\n\x1a\n_rest_of_file";

    #[test]
    fn sniff_common_formats() {
        assert_eq!(sniff_mime(b"\xFF\xD8\xFF\xE0xxxx"), "image/jpeg");
        assert_eq!(sniff_mime(PNG_HEAD), "image/png");
        assert_eq!(sniff_mime(b"GIF89a"), "image/gif");
        assert_eq!(sniff_mime(b"RIFF\x00\x00\x00\x00WEBPVP8 "), "image/webp");
        assert_eq!(sniff_mime(b"unknown"), "image/jpeg");
    }

    #[test]
    fn normalize_passes_urls_and_rebuilds_base64() {
        assert_eq!(
            normalize_input("https://example.com/a.jpg", 1024).unwrap(),
            "https://example.com/a.jpg"
        );
        let encoded = BASE64.encode(PNG_HEAD);
        let uri = normalize_input(&encoded, 1024).unwrap();
        assert!(uri.starts_with("data:image/png;base64,"));
        // data URI 输入按嗅探结果重建 MIME（声明的 image/jpeg 被纠正为 png）。
        let uri = normalize_input(&format!("data:image/jpeg;base64,{encoded}"), 1024).unwrap();
        assert!(uri.starts_with("data:image/png;base64,"));
    }

    #[test]
    fn normalize_rejects_bad_input() {
        assert!(normalize_input("", 1024).is_err());
        assert!(normalize_input("不是base64!!!", 1024).is_err());
        assert!(normalize_input(&BASE64.encode(PNG_HEAD), 4).is_err()); // 超大小上限
        assert!(normalize_input("data:image/png,notbase64", 1024).is_err()); // 非 base64 data URI
    }
}

/// Functions for loading resources (platform independent)
use cfg_if::cfg_if;

#[cfg(target_arch = "wasm32")]
const CRATE_LOCATION: &str = "";

#[cfg(target_arch = "wasm32")]
fn format_url(file_name: &str) -> reqwest::Url {
    use anyhow::anyhow;

    let window = web_sys::window().unwrap();
    let location = window.location();
    let origin = location.origin().unwrap();
    reqwest::Url::parse(&format!("{}/{}", origin, CRATE_LOCATION))
        .unwrap()
        .join(file_name)
        .unwrap()
}

pub async fn load_bytes(filename: &str) -> anyhow::Result<Vec<u8>> {
    cfg_if! {
        if #[cfg(target_arch="wasm32")] {
            let url = format_url(filename);
            log::info!("requesting {url}");
            let data = reqwest::get(url)
                .await?
                .bytes()
                .await?
                .to_vec();
        } else {
            let data = tokio::fs::read(filename).await?;
        }
    }

    Ok(data)
}

pub async fn load_string(filename: &str) -> anyhow::Result<String> {
    cfg_if! {
        if #[cfg(target_arch="wasm32")] {
            let url = format_url(filename);
            log::info!("requesting {url}");
            let data = reqwest::get(url)
                .await?
                .text()
                .await?;
        } else {
            let data = tokio::fs::read_to_string(filename).await?;
        }
    }

    Ok(data)
}

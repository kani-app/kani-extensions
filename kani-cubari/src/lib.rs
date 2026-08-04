#[cfg(not(target_family = "wasm"))]
compile_error!(
    "kani-extensions/* are WASM-only -- build with `cargo run -p kani-cli -- build <name>`. \
     If a tool triggered this, it is using --workspace (or defaulting to it); scope it to \
     default-members instead -- clippy/nextest omit the flag, cargo-dist needs precise-builds."
);

use kani_shared::bindings::exports::kani::extension::manga_provider::Guest;
use kani_shared::host_abi::{HttpRequest, JsonHandle, prefs};
use kani_shared::{
    ExtensionError, ExtensionMetadata, ExtensionResult, MangaExtension, MangaStatus, bindings,
    decode_manga_id, encode_manga_id, ext_version, to_shared_filters, types::ActiveFilter,
    wit_types,
};
use wit_types::{
    Chapter, ChapterInfo, ChapterList, MangaInfo, MangaList, MangaListItem, Page, PreferenceSpec,
};

kani_shared::guest_alloc!();

const CUBARI_BASE: &str = "https://cubari.moe";

pub struct Cubari;

impl Cubari {
    pub fn new() -> Self {
        Self
    }

    pub fn metadata() -> ExtensionMetadata {
        ExtensionMetadata {
            id: "cubari".to_string(),
            name: "Cubari".to_string(),
            version: ext_version!("0.1.0"),
            base_url: CUBARI_BASE.to_string(),
            language: "multi".to_string(),
            nsfw: false,
            unrestricted_http: true,
            mihon_source_id: None,
            rate_limit: None,
            ..Default::default()
        }
    }

    /// Fetch and parse a manifest from a raw URL.
    fn fetch_manifest(url: &str) -> ExtensionResult<JsonHandle> {
        HttpRequest::get(url)
            .header("User-Agent", "kani-cubari/0.1")
            .send_json_handle()
    }

    /// Reads manifest URLs from the stored preference (JSON array).
    fn manifest_urls() -> Vec<String> {
        let raw = prefs::get_str_or("manifest_urls", "[]");
        JsonHandle::parse(raw.as_bytes())
            .ok()
            .map(|json| {
                let count = json.array_len("").unwrap_or(0);
                (0..count)
                    .filter_map(|i| json.array_get("", i).ok()?.get_str(""))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Resolve a group value to a list of image URLs.
    fn resolve_group_pages(group_value: &str) -> ExtensionResult<Vec<Page>> {
        let fetch_url = if group_value.starts_with('/') {
            format!("{}{}", CUBARI_BASE, group_value)
        } else {
            group_value.to_string()
        };

        let json = HttpRequest::get(&fetch_url)
            .header("User-Agent", "kani-cubari/0.1")
            .send_json_handle()?;

        let count = json.array_len("/data").or_else(|| json.array_len(""));
        let (ptr, src_ptr) = if json.array_len("/data").is_some() {
            ("/data", "/src")
        } else {
            ("", "")
        };

        let count = count.unwrap_or(0);
        let pages = (0..count)
            .filter_map(|i| {
                let item = json.array_get(ptr, i).ok()?;
                let url = if src_ptr.is_empty() {
                    item.get_str("")
                } else {
                    item.get_str(src_ptr)
                }?;
                Some(Page {
                    index: i,
                    url,
                    transform: None,
                })
            })
            .collect();

        Ok(pages)
    }
}

impl Default for Cubari {
    fn default() -> Self {
        Self::new()
    }
}

impl Guest for Cubari {
    fn get_metadata() -> Result<String, wit_types::ExtensionError> {
        Ok(kani_shared::serde_json::to_string(&Cubari::metadata())
            .expect("ExtensionMetadata serializes to JSON"))
    }

    fn get_popular_manga(
        page: i32,
        page_size: i32,
        filters: Vec<wit_types::ActiveFilter>,
    ) -> Result<MangaList, wit_types::ExtensionError> {
        let shared = to_shared_filters(filters);
        get_extension()
            .get_popular_manga(page, page_size, &shared)
            .map_err(|e| e.into_wit())
    }

    fn search_manga(
        query: String,
        page: i32,
        page_size: i32,
        filters: Vec<wit_types::ActiveFilter>,
    ) -> Result<MangaList, wit_types::ExtensionError> {
        let shared = to_shared_filters(filters);
        get_extension()
            .search_manga(&query, page, page_size, &shared)
            .map_err(|e| e.into_wit())
    }

    fn get_filter_list() -> Result<wit_types::FilterList, wit_types::ExtensionError> {
        get_extension().get_filter_list().map_err(|e| e.into_wit())
    }

    fn get_fetched_option_sets() -> Result<String, wit_types::ExtensionError> {
        get_extension()
            .get_fetched_option_sets()
            .map_err(|e| e.into_wit())
    }

    fn get_manga_details(manga_id: String) -> Result<MangaInfo, wit_types::ExtensionError> {
        get_extension()
            .get_manga_details(&manga_id)
            .map_err(|e| e.into_wit())
    }

    fn get_chapter_list(
        manga_id: String,
        page: i32,
        page_size: Option<i32>,
        sort: Option<String>,
    ) -> Result<ChapterList, wit_types::ExtensionError> {
        get_extension()
            .get_chapter_list(&manga_id, page, page_size, sort)
            .map_err(|e| e.into_wit())
    }
    async fn get_chapter_list_stream(
        manga_id: String,
        sort: Option<String>,
    ) -> kani_shared::StreamReader<Result<ChapterInfo, wit_types::ExtensionError>> {
        kani_shared::bridge_chapter_list_stream(get_extension(), manga_id, sort)
    }

    fn get_chapter_sort_list() -> Result<Vec<wit_types::SortOption>, wit_types::ExtensionError> {
        get_extension()
            .get_chapter_sort_list()
            .map_err(|e| e.into_wit())
    }

    fn get_pages(
        manga_id: String,
        chapter_id: String,
    ) -> Result<Chapter, wit_types::ExtensionError> {
        get_extension()
            .get_pages(&manga_id, &chapter_id)
            .map_err(|e| e.into_wit())
    }
    fn get_preferences() -> Result<Vec<PreferenceSpec>, wit_types::ExtensionError> {
        get_extension().get_preferences().map_err(|e| e.into_wit())
    }
    fn get_url(manga_id: String) -> Result<String, wit_types::ExtensionError> {
        get_extension().get_url(&manga_id).map_err(|e| e.into_wit())
    }
}

impl MangaExtension for Cubari {
    fn name(&self) -> &str {
        "Cubari"
    }

    fn get_popular_manga(
        &self,
        _page: i32,
        _page_size: i32,
        _filters: &[ActiveFilter],
    ) -> ExtensionResult<MangaList> {
        let manga = Self::manifest_urls()
            .into_iter()
            .filter_map(|url| {
                let json = Self::fetch_manifest(&url).ok()?;
                let title = json.get_str("/title").unwrap_or_else(|| url.clone());
                let cover_url = json.get_str("/cover");
                Some(MangaListItem {
                    id: encode_manga_id(&url),
                    title,
                    cover_url,
                })
            })
            .collect();

        Ok(MangaList {
            manga,
            has_next_page: false,
            total_pages: None,
        })
    }

    fn search_manga(
        &self,
        query: &str,
        _page: i32,
        _page_size: i32,
        _filters: &[ActiveFilter],
    ) -> ExtensionResult<MangaList> {
        let q = query.to_lowercase();
        let manga = Self::manifest_urls()
            .into_iter()
            .filter_map(|url| {
                let json = Self::fetch_manifest(&url).ok()?;
                let title = json.get_str("/title").unwrap_or_else(|| url.clone());
                if !title.to_lowercase().contains(&q) {
                    return None;
                }
                let cover_url = json.get_str("/cover");
                Some(MangaListItem {
                    id: encode_manga_id(&url),
                    title,
                    cover_url,
                })
            })
            .collect();

        Ok(MangaList {
            manga,
            has_next_page: false,
            total_pages: None,
        })
    }

    fn get_manga_details(&self, manga_id: &str) -> ExtensionResult<MangaInfo> {
        let url = decode_manga_id(manga_id);
        let json = Self::fetch_manifest(&url)?;

        Ok(MangaInfo {
            id: manga_id.to_string(),
            title: json
                .get_str("/title")
                .unwrap_or_else(|| manga_id.to_string()),
            description: json.get_str("/description"),
            cover_url: json.get_str("/cover"),
            status: MangaStatus::Unknown,
            authors: json.get_str("/author").into_iter().collect(),
            artists: json.get_str("/artist").into_iter().collect(),
            tags: vec![],
        })
    }

    fn get_chapter_list(
        &self,
        manga_id: &str,
        _page: i32,
        _page_size: Option<i32>,
        _sort: Option<String>,
    ) -> ExtensionResult<ChapterList> {
        let url = decode_manga_id(manga_id);
        let json = Self::fetch_manifest(&url)?;

        let mut chapters: Vec<ChapterInfo> = json
            .object_keys("/chapters")
            .into_iter()
            .filter_map(|ch_key| {
                let number: f64 = ch_key.parse().ok()?;
                let ch = json.object_get("/chapters", &ch_key)?;
                let title = ch.get_str("/title");
                let date_uploaded = ch.get_i64("/last_updated");

                let group_keys = ch.object_keys("/groups");
                let scanlator = group_keys.into_iter().next();

                let raw_chapter_id = format!("{}|{}", url, ch_key);

                Some(ChapterInfo {
                    id: encode_manga_id(&raw_chapter_id),
                    number,
                    title,
                    volume: None,
                    scanlator,
                    date_uploaded,
                    language: "en".to_string(),
                    page_count: None,
                })
            })
            .collect();

        chapters.sort_by(|a, b| {
            a.number
                .partial_cmp(&b.number)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(ChapterList {
            chapters,
            has_next_page: false,
            total_pages: None,
        })
    }

    fn get_pages(&self, _manga_id: &str, chapter_id: &str) -> ExtensionResult<Chapter> {
        let raw_chapter_id = decode_manga_id(chapter_id);
        let sep = raw_chapter_id
            .rfind('|')
            .ok_or_else(|| ExtensionError::parse("Invalid chapter ID".into()))?;
        let manifest_url = &raw_chapter_id[..sep];
        let ch_key = &raw_chapter_id[sep + 1..];

        let json = Self::fetch_manifest(manifest_url)?;

        let ch = json
            .object_get("/chapters", ch_key)
            .ok_or_else(|| ExtensionError::not_found(format!("Chapter {} not found", ch_key)))?;

        let group_keys = ch.object_keys("/groups");
        let first_group = group_keys
            .first()
            .ok_or_else(|| ExtensionError::parse("Chapter has no groups".into()))?;

        let group_value = ch
            .object_get("/groups", first_group)
            .and_then(|v| v.get_str(""))
            .ok_or_else(|| ExtensionError::parse("Missing group value".into()))?;

        let pages = Self::resolve_group_pages(&group_value)?;
        Ok(Chapter { pages })
    }

    fn get_filter_list(&self) -> ExtensionResult<wit_types::FilterList> {
        Ok(wit_types::FilterList { filters: vec![] })
    }

    fn get_preferences(&self) -> ExtensionResult<Vec<PreferenceSpec>> {
        use kani_shared::preference_list;
        Ok(preference_list! {
            "manifest_urls", "Manifest URLs", MultiValueList,
                placeholder: "https://raw.githubusercontent.com/user/repo/main/manifest.json",
                description: "Raw URLs to Cubari-format JSON manifests. \
                              Paste the direct URL to each JSON file.";
        })
    }

    fn get_chapter_sort_list(&self) -> ExtensionResult<Vec<wit_types::SortOption>> {
        Ok(vec![])
    }

    fn get_url(&self, manga_id: &str) -> ExtensionResult<String> {
        // manga_id is a base64-encoded URL; decode it to get the canonical cubari URL
        Ok(decode_manga_id(manga_id))
    }
}

// ============================================================
// WASM Exports
// ============================================================
// These functions are the entry points called by the host when
// this extension is loaded as a WASM module.

use std::sync::OnceLock;

static EXTENSION: OnceLock<Cubari> = OnceLock::new();

fn get_extension() -> &'static Cubari {
    EXTENSION.get_or_init(Cubari::new)
}

bindings::export!(Cubari);

#[cfg(not(target_family = "wasm"))]
compile_error!(
    "kani-extensions/* are WASM-only -- build with `cargo run -p kani-cli -- build <name>`. \
     If a tool triggered this, it is using --workspace (or defaulting to it); scope it to \
     default-members instead -- clippy/nextest omit the flag, cargo-dist needs precise-builds."
);

use kani_shared::ast::{BlueprintBuilder, Expr, OffsetType};
use kani_shared::bindings::exports::kani::extension::manga_provider::Guest;
use kani_shared::host_abi::{HttpRequest, extract};
use kani_shared::wit_types::ChapterInfo;
use kani_shared::{
    ApplyFilters, ExtensionMetadata, ExtensionResult, MangaExtension, MangaStatus, bindings,
    ext_version, filter_list, to_shared_filters, types::ActiveFilter, wit_types,
};
use wit_types::{Chapter, ChapterList, MangaInfo, MangaList, MangaListItem, PreferenceSpec};

kani_shared::guest_alloc!();

pub struct Mangapill {
    base_url: String,
}

impl Default for Mangapill {
    fn default() -> Self {
        Self::new()
    }
}

impl Mangapill {
    pub fn new() -> Self {
        Self {
            base_url: "https://mangapill.com".to_string(),
        }
    }

    pub fn metadata() -> ExtensionMetadata {
        ExtensionMetadata {
            id: "mangapill".to_string(),
            name: "Mangapill".to_string(),
            version: ext_version!("0.1.0"),
            base_url: "https://mangapill.com".to_string(),
            language: "multi".to_string(),
            nsfw: false,
            unrestricted_http: false,
            mihon_source_id: Some(8448310129093543312_i64),
            rate_limit: None,
            ..Default::default()
        }
    }

    fn fetch_manga_list(
        &self,
        query: &str,
        page: i32,
        page_size: i32,
        filters: &[ActiveFilter],
    ) -> ExtensionResult<MangaList> {
        let req = HttpRequest::get(format!("{}/search", self.base_url))
            .query("q", query)
            .apply_filters(filters);

        let bp = BlueprintBuilder::new(".grid.gap-3 > div")
            .request(req)
            .field(
                "id",
                Expr::self_ref().first("a").attr("href").split("/").at(2),
            )
            .field("title", Expr::self_ref().first(".line-clamp-2").text())
            .field_opt("cover_url", Expr::self_ref().first("img").attr("data-src"))
            .paginated(50, "page", OffsetType::PageNumber { start: 1 })
            .build();

        let rows = extract::paginated_html(page, page_size, &bp)?;
        let has_next_page = rows.get_scalar_bool("has_next_page");

        let manga = rows
            .rows_iter()
            .filter_map(|row| {
                Some(MangaListItem {
                    id: row.require_str("/id").ok()?,
                    title: row.require_str("/title").ok()?,
                    cover_url: row.get_str("/cover_url"),
                })
            })
            .collect();

        Ok(MangaList {
            manga,
            has_next_page,
            total_pages: None,
        })
    }
}

impl Guest for Mangapill {
    fn get_metadata() -> Result<String, wit_types::ExtensionError> {
        Ok(kani_shared::serde_json::to_string(&Mangapill::metadata())
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

impl MangaExtension for Mangapill {
    fn name(&self) -> &str {
        "Mangapill"
    }

    fn get_popular_manga(
        &self,
        page: i32,
        page_size: i32,
        filters: &[ActiveFilter],
    ) -> ExtensionResult<MangaList> {
        if filters.is_empty() {
            return Ok(MangaList {
                manga: vec![],
                has_next_page: false,
                total_pages: None,
            });
        }
        self.fetch_manga_list("", page, page_size, filters)
    }

    fn search_manga(
        &self,
        query: &str,
        page: i32,
        page_size: i32,
        filters: &[ActiveFilter],
    ) -> ExtensionResult<MangaList> {
        self.fetch_manga_list(query, page, page_size, filters)
    }

    fn get_manga_details(&self, manga_id: &str) -> ExtensionResult<MangaInfo> {
        let bp = BlueprintBuilder::new(":root")
            .request(HttpRequest::get(format!(
                "{}/manga/{}",
                self.base_url, manga_id
            )))
            .field("title", Expr::dom("h1.font-bold").text())
            .field_opt("description", Expr::dom("p.text-sm").text())
            .field(
                "status",
                Expr::dom("div.grid:nth-child(3) > div:nth-child(2) > div:nth-child(2)")
                    .text()
                    .lookup(vec![
                        ("publishing", "ongoing"),
                        ("finished", "completed"),
                        ("on hiatus", "hiatus"),
                        ("discontinued", "cancelled"),
                        ("not yet published", "hiatus"),
                    ])
                    .fallback_str("unknown"),
            )
            .field_opt("cover_url", Expr::dom("img").attr("data-src"))
            .field(
                "tags",
                Expr::merge(vec![
                    Expr::self_ref()
                        .select("div.mb-3 a.text-sm")
                        .map(Expr::var("$item").text().trim()),
                    Expr::self_ref()
                        .select("div.grid:nth-child(3) > div:nth-child(1) > div:nth-child(2)")
                        .map(Expr::var("$item").text().trim()),
                    Expr::self_ref()
                        .select("div.grid:nth-child(3) > div:nth-child(3) > div:nth-child(2)")
                        .map(Expr::var("$item").text().trim()),
                ]),
            )
            .build();

        let rows = extract::html(None, &bp)?;
        let row = rows
            .rows_get(0)
            .map_err(|_| kani_shared::ExtensionError::parse("no details row".into()))?;

        let status = match row.get_str("/status").as_deref() {
            Some("ongoing") => MangaStatus::Ongoing,
            Some("completed") => MangaStatus::Completed,
            Some("hiatus") => MangaStatus::Hiatus,
            Some("cancelled") => MangaStatus::Cancelled,
            _ => MangaStatus::Unknown,
        };

        Ok(MangaInfo {
            id: manga_id.to_string(),
            title: row.require_str("/title")?,
            description: row.get_str("/description"),
            status,
            authors: vec![],
            artists: vec![],
            tags: row.get_array_of_strings("/tags"),
            cover_url: row.get_str("/cover_url"),
        })
    }

    fn get_chapter_list(
        &self,
        manga_id: &str,
        _page: i32,
        _page_size: Option<i32>,
        _sort: Option<String>,
    ) -> ExtensionResult<ChapterList> {
        let bp = BlueprintBuilder::new("div.grid a.border")
            .request(HttpRequest::get(format!(
                "{}/manga/{}",
                self.base_url, manga_id
            )))
            .field("id", Expr::self_ref().attr("href").split("/").at(2))
            .field(
                "number",
                Expr::self_ref()
                    .text()
                    .trim()
                    .split(" ")
                    .at(-1)
                    .parse_float()
                    .fallback(Expr::num(0.0)),
            )
            .build();

        let rows = extract::html(None, &bp)?;
        let count = rows.rows_len();
        let chapters = (0..count)
            .filter_map(|i| {
                let row = rows.rows_get(i).ok()?;
                Some(ChapterInfo {
                    id: row.require_str("/id").ok()?,
                    number: row.get_f64("/number").unwrap_or(0.0),
                    title: None,
                    volume: None,
                    scanlator: None,
                    date_uploaded: None,
                    language: "en".to_string(),
                    page_count: None,
                })
            })
            .collect();

        Ok(ChapterList {
            chapters,
            has_next_page: false,
            total_pages: None,
        })
    }

    fn get_pages(&self, _manga_id: &str, chapter_id: &str) -> ExtensionResult<Chapter> {
        let bp = BlueprintBuilder::new("img.js-page")
            .request(HttpRequest::get(format!(
                "{}/chapters/{}",
                self.base_url, chapter_id
            )))
            .field("url", Expr::self_ref().attr("data-src"))
            .field("index", Expr::index())
            .build();

        let rows = extract::html(None, &bp)?;
        let count = rows.rows_len();
        Ok(Chapter {
            pages: (0..count)
                .filter_map(|i| {
                    let row = rows.rows_get(i).ok()?;
                    Some(wit_types::Page {
                        index: row.get_i64("/index").unwrap_or(i as i64) as i32,
                        url: row.require_str("/url").ok()?,
                        transform: None,
                    })
                })
                .collect(),
        })
    }

    fn get_chapter_sort_list(&self) -> ExtensionResult<Vec<wit_types::SortOption>> {
        Ok(vec![])
    }

    fn get_filter_list(&self) -> ExtensionResult<wit_types::FilterList> {
        Ok(filter_list! {
            "genre", "Genre", Multiselect, [
                "Action", "Adventure", "Cars", "Comedy", "Dementia", "Demons",
                "Doujinshi", "Drama", "Ecchi", "Fantasy", "Game", "Gender Bender",
                "Harem", "Historical", "Horror", "Isekai", "Josei", "Kids", "Magic",
                "Martial Arts", "Mecha", "Military", "Music", "Mystery", "Parody",
                "Police", "Psychological", "Romance", "Samurai", "School", "Sci-Fi",
                "Seinen", "Shoujo", "Shoujo Ai", "Shounen", "Shounen Ai",
                "Slice of Life", "Space", "Sports", "Super Power", "Supernatural",
                "Thriller", "Tragedy", "Vampire", "Yaoi", "Yuri"
            ];
            "type", "Type", Select, [("All", ""), ("Manga", "manga"), ("Manhua", "manhua"), ("Novel", "novel"), ("One-Shot", "one-shot"), ("Doujinshi", "doujinshi"), ("OEL", "oel")], default: ("All", "");
            "status", "Status", Select, [("All", ""), ("Ongoing", "publishing"), ("Completed", "finished"), ("On Hiatus", "on+hiatus"), ("Discontinued", "discontinued"), ("Not Yet Published", "not+yet+published")], default: ("All", "");
        })
    }

    fn get_preferences(&self) -> ExtensionResult<Vec<PreferenceSpec>> {
        Ok(vec![])
    }

    fn get_url(&self, manga_id: &str) -> ExtensionResult<String> {
        Ok(format!("https://mangapill.com/manga/{}", manga_id))
    }
}

// ============================================================
// WASM Exports
// ============================================================
use std::sync::OnceLock;

static EXTENSION: OnceLock<Mangapill> = OnceLock::new();

fn get_extension() -> &'static Mangapill {
    EXTENSION.get_or_init(Mangapill::new)
}

bindings::export!(Mangapill);

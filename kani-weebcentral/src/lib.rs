#[cfg(not(target_family = "wasm"))]
compile_error!(
    "kani-extensions/* are WASM-only -- build with `cargo run -p kani-cli -- build <name>`. \
     If a tool triggered this, it is using --workspace (or defaulting to it); scope it to \
     default-members instead -- clippy/nextest omit the flag, cargo-dist needs precise-builds."
);

use kani_shared::ExtensionError;
use kani_shared::ast::{BlueprintBuilder, Expr, OffsetType};
use kani_shared::bindings::exports::kani::extension::manga_provider::Guest;
use kani_shared::wit_types::{ChapterInfo, MangaListItem, Page};
use kani_shared::{
    ExtensionMetadata, ExtensionResult, FilterGroups, MangaExtension, MangaStatus, bindings,
    ext_version, filter_list,
    host_abi::{HttpRequest, extract},
    to_shared_filters,
    types::ActiveFilter,
    wit_types,
};
use wit_types::{Chapter, ChapterList, MangaInfo, MangaList, PreferenceSpec};

kani_shared::guest_alloc!();

pub struct WeebCentral {
    base_url: String,
}

impl Default for WeebCentral {
    fn default() -> Self {
        Self::new()
    }
}

impl WeebCentral {
    pub fn new() -> Self {
        Self {
            base_url: "https://weebcentral.com".to_string(),
        }
    }

    pub fn metadata() -> ExtensionMetadata {
        ExtensionMetadata {
            id: "weebcentral".to_string(),
            name: "Weeb Central".to_string(),
            version: ext_version!("0.1.0"),
            base_url: "https://weebcentral.com".to_string(),
            language: "multi".to_string(),
            nsfw: false,
            unrestricted_http: false,
            mihon_source_id: Some(2131019126180322627_i64),
            rate_limit: None,
            ..Default::default()
        }
    }

    fn fetch_manga_list(
        &self,
        req: HttpRequest,
        page: i32,
        page_size: i32,
    ) -> ExtensionResult<MangaList> {
        let bp = BlueprintBuilder::new("body > article")
            .request(req)
            .field(
                "id",
                Expr::self_ref()
                    .first("a.line-clamp-1")
                    .attr("href")
                    .split("/")
                    .at(-2),
            )
            .field("title", Expr::self_ref().first("a.line-clamp-1").text())
            .field_opt("cover_url", Expr::self_ref().first("img").attr("src"))
            .scalar(
                "has_next_page",
                Expr::dom(".col-span-2")
                    .text()
                    .matches(".+")
                    .fallback(Expr::false_val()),
            )
            .paginated(32, "offset", OffsetType::ItemOffset)
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

impl Guest for WeebCentral {
    fn get_metadata() -> Result<String, wit_types::ExtensionError> {
        Ok(kani_shared::serde_json::to_string(&WeebCentral::metadata())
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

    fn get_pages(
        manga_id: String,
        chapter_id: String,
    ) -> Result<Chapter, wit_types::ExtensionError> {
        get_extension()
            .get_pages(&manga_id, &chapter_id)
            .map_err(|e| e.into_wit())
    }

    fn get_chapter_sort_list() -> Result<Vec<wit_types::SortOption>, wit_types::ExtensionError> {
        get_extension()
            .get_chapter_sort_list()
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

    fn get_preferences() -> Result<Vec<PreferenceSpec>, wit_types::ExtensionError> {
        get_extension().get_preferences().map_err(|e| e.into_wit())
    }
    fn get_url(manga_id: String) -> Result<String, wit_types::ExtensionError> {
        get_extension().get_url(&manga_id).map_err(|e| e.into_wit())
    }
}

impl MangaExtension for WeebCentral {
    fn name(&self) -> &str {
        "Weeb Central"
    }

    fn get_popular_manga(
        &self,
        page: i32,
        page_size: i32,
        _filters: &[ActiveFilter],
    ) -> ExtensionResult<MangaList> {
        let req = HttpRequest::get(format!("{}/search/data", self.base_url))
            .query("sort", "Popularity")
            .query("order", "Descending")
            .query("official", "Any")
            .query("anime", "Any")
            .query("adult", "Any")
            .query("display_mode", "Full Display");

        self.fetch_manga_list(req, page, page_size)
    }

    fn search_manga(
        &self,
        query: &str,
        page: i32,
        page_size: i32,
        filters: &[ActiveFilter],
    ) -> ExtensionResult<MangaList> {
        let fg = FilterGroups::from(filters);

        let mut req = HttpRequest::get(format!("{}/search/data", self.base_url))
            .query("display_mode", "Full Display");

        if !query.is_empty() {
            req = req.query("text", query);
        }

        for group in ["included_tag", "included_type", "included_status"] {
            for v in fg.multiselect_values(group) {
                req = req.query(group, v);
            }
        }

        for group in ["official", "anime", "adult"] {
            if let Some(val) = fg.selection_value(group) {
                req = req.query(group, val);
            }
        }

        // Sort encodes key and direction as a single value (e.g. "Popularity");
        // direction is always Descending for this API's simple sort options.
        if let Some(sort_val) = fg.selection_value("sort") {
            let (sort, order) = sort_val.split_once(':').unwrap_or((sort_val, "desc"));
            req = req.query("sort", sort).query(
                "order",
                if order == "desc" {
                    "Descending"
                } else {
                    "Ascending"
                },
            );
        }

        self.fetch_manga_list(req, page, page_size)
    }

    fn get_manga_details(&self, manga_id: &str) -> ExtensionResult<MangaInfo> {
        let bp = BlueprintBuilder::new(":root")
            .request(HttpRequest::get(format!(
                "{}/series/{}",
                &self.base_url, manga_id
            )))
            .field("title", Expr::dom("h1.hidden").text())
            .field_opt("description", Expr::dom(".whitespace-pre-wrap").text())
            .field(
                "status",
                Expr::dom("ul.flex-col li:nth-child(4) a")
                    .text()
                    .lookup(vec![
                        ("Ongoing", "ongoing"),
                        ("Completed", "completed"),
                        ("Hiatus", "hiatus"),
                        ("Cancelled", "cancelled"),
                    ])
                    .fallback_str("unknown"),
            )
            .field_opt(
                "cover_url",
                Expr::dom("section.flex:nth-child(3) > picture:nth-child(1) > img:nth-child(2)")
                    .attr("src"),
            )
            .field(
                "authors",
                Expr::self_ref()
                    .select("ul.flex-col li:first-child span a")
                    .map(Expr::var("$item").text().trim()),
            )
            .field(
                "tags",
                Expr::merge(vec![
                    Expr::self_ref()
                        .select("ul.flex-col li:nth-child(2) span a")
                        .map(Expr::var("$item").text().trim()),
                    Expr::self_ref()
                        .select("ul.flex-col li:nth-child(3) a")
                        .map(Expr::var("$item").text().trim()),
                    Expr::self_ref()
                        .select("ul.flex-col li:nth-child(5) span")
                        .map(Expr::var("$item").text().trim()),
                ]),
            )
            .build();

        let rows = extract::html(None, &bp)?;
        let row = rows
            .rows_get(0)
            .map_err(|_| ExtensionError::parse("no details row".into()))?;

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
            authors: row.get_array_of_strings("/authors"),
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
        let bp = BlueprintBuilder::new("body > div")
            .request(HttpRequest::get(format!(
                "{}/series/{}/full-chapter-list",
                &self.base_url, manga_id
            )))
            .field(
                "id",
                Expr::self_ref().first("a").attr("href").split("/").at(-1),
            )
            .field(
                "number",
                Expr::self_ref()
                    .first("span.gap-2 span")
                    .text()
                    .trim()
                    .split(" ")
                    .at(-1)
                    .parse_float()
                    .fallback(Expr::num(0.0)),
            )
            .field_opt(
                "date_uploaded",
                Expr::self_ref()
                    .first("time")
                    .attr("datetime")
                    .date_parse_rfc3339(),
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
                    date_uploaded: row.get_i64("/date_uploaded"),
                    title: None,
                    volume: None,
                    scanlator: None,
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
        let bp = BlueprintBuilder::new("section img")
            .request(
                HttpRequest::get(format!("{}/chapters/{}/images", &self.base_url, chapter_id))
                    .query("reading_style", "long_strip"),
            )
            .field("url", Expr::self_ref().attr("src"))
            .field("index", Expr::index())
            .build();

        let rows = extract::html(None, &bp)?;
        let count = rows.rows_len();
        Ok(Chapter {
            pages: (0..count)
                .filter_map(|i| {
                    let row = rows.rows_get(i).ok()?;
                    Some(Page {
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
            "sort", "Sort", Sort, ["Best Match", "Alphabet", "Popularity", "Subscribers", "Recently Added", "Latest Updates"];
            "official", "Official Translation", Select, ["Any", "True", "False"], default: "Any";
            "anime", "Anime Adaptation", Select, ["Any", "True", "False"], default: "Any";
            "adult", "Adult Content", Select, ["Any", "True", "False"], default: "Any";
            "included_status", "Series Status", Multiselect, ["Ongoing", "Completed", "Hiatus", "Cancelled"];
            "included_type", "Series Type", Multiselect, ["Manga", "Manhwa", "Manhua", "OEL"];
            "included_tag", "Tags", Multiselect, [
                "Action", "Adult", "Adventure", "Comedy", "Doujinshi", "Drama",
                "Ecchi", "Fantasy", "Gender Bender", "Harem", "Hentai", "Historical",
                "Horror", "Isekai", "Josei", "Lolicon", "Martial Arts", "Mature",
                "Mecha", "Mystery", "Psychological", "Romance", "School Life", "Sci-fi",
                "Seinen", "Shotacon", "Shoujo", "Shoujo Ai", "Shounen", "Shounen Ai",
                "Slice of Life", "Smut", "Sports", "Supernatural", "Tragedy",
                "Yaoi", "Yuri", "Other"
            ];
        })
    }

    fn get_preferences(&self) -> ExtensionResult<Vec<PreferenceSpec>> {
        Ok(vec![])
    }

    fn get_url(&self, manga_id: &str) -> ExtensionResult<String> {
        Ok(format!("https://weebcentral.com/series/{}", manga_id))
    }
}

// ============================================================
// WASM Exports
// ============================================================
// These functions are the entry points called by the host when
// this extension is loaded as a WASM module.

use std::sync::OnceLock;

static EXTENSION: OnceLock<WeebCentral> = OnceLock::new();

fn get_extension() -> &'static WeebCentral {
    EXTENSION.get_or_init(WeebCentral::new)
}

bindings::export!(WeebCentral);

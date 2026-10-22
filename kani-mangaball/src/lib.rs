#[cfg(not(target_family = "wasm"))]
compile_error!(
    "kani-extensions/* are WASM-only -- build with `cargo run -p kani-cli -- build <name>`. \
     If a tool triggered this, it is using --workspace (or defaulting to it); scope it to \
     default-members instead -- clippy/nextest omit the flag, cargo-dist needs precise-builds."
);

use kani_shared::ast::{BlueprintBuilder, Expr};
use kani_shared::bindings::exports::kani::extension::manga_provider::Guest;
use kani_shared::host_abi::{HttpRequest, extract, prefs};
use kani_shared::serde_json::Value;
use kani_shared::{
    ExtensionError, ExtensionMetadata, ExtensionResult, FilterGroups, MangaExtension, MangaStatus,
    bindings, ext_version, filter_list, to_shared_filters, types::ActiveFilter, wit_types,
};
use wit_types::{
    Chapter, ChapterInfo, ChapterList, MangaInfo, MangaList, MangaListItem, Page, PreferenceSpec,
};

kani_shared::guest_alloc!();

const BASE_URL: &str = "https://mangaball.com";
const COVER_BASE: &str = "https://bulbasaur.poke-black-and-white.net/covers/";
const LANGUAGE: &str = "en";
const MAX_PAGE_SIZE: i32 = 100;

pub struct MangaBall;

impl Default for MangaBall {
    fn default() -> Self {
        Self::new()
    }
}

/// The cover the site itself shows: its own copy first, then the mirrors it falls back to.
fn cover_expr(image: Expr) -> Expr {
    let own = image
        .clone()
        .ptr("/cover/path")
        .str_val()
        .replace("\\", "/")
        .trim();
    let own = Expr::if_then_else(
        own.clone().starts_with("http"),
        own.clone(),
        own.prepend_str(COVER_BASE),
    );
    Expr::if_then_else(
        image
            .clone()
            .ptr("/cover/path")
            .str_val()
            .fallback_str("")
            .ne(Expr::lit("")),
        own,
        image
            .clone()
            .ptr("/file")
            .str_val()
            .fallback(image.clone().ptr("/cdn_mangadex").str_val())
            .fallback(image.clone().ptr("/cdn_mangaupdate").str_val())
            .fallback(image.ptr("/cdn_mangaupdates").str_val()),
    )
}

/// `cover_expr` for a document parsed in the guest.
fn cover_from(image: &Value) -> Option<String> {
    let text = |p: &str| {
        image
            .pointer(p)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    if let Some(path) = text("/cover/path") {
        let path = path.replace('\\', "/");
        return Some(if path.starts_with("http") {
            path
        } else {
            format!("{COVER_BASE}{path}")
        });
    }
    [
        "/file",
        "/cdn_mangadex",
        "/cdn_mangaupdate",
        "/cdn_mangaupdates",
    ]
    .into_iter()
    .find_map(text)
    .map(String::from)
}

fn status_of(raw: Option<&str>) -> MangaStatus {
    match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("ongoing") => MangaStatus::Ongoing,
        Some("completed") => MangaStatus::Completed,
        Some("hiatus") => MangaStatus::Hiatus,
        Some("cancelled") | Some("canceled") => MangaStatus::Cancelled,
        _ => MangaStatus::Unknown,
    }
}

/// Long series list thousands of uploads, more than the declarative evaluator's step budget
/// allows, so chapter lists are parsed here instead.
fn fetch_json(req: HttpRequest) -> ExtensionResult<Value> {
    let resp = req.send()?;
    if !(200..300).contains(&resp.status) {
        return Err(ExtensionError::network(format!("HTTP {}", resp.status)));
    }
    kani_shared::serde_json::from_slice(&resp.body)
        .map_err(|e| ExtensionError::parse(format!("invalid JSON: {e}")))
}

/// `2026-10-16T17:33:55.645000`, a UTC time without an offset.
fn parse_timestamp(raw: &str) -> Option<i64> {
    let b = raw.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| raw.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hh, mm, ss) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hh * 3_600 + mm * 60 + ss)
}

fn is_object_id(id: &str) -> bool {
    id.len() == 24 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A chapter's name is often just its number; that adds nothing to the number Kani shows.
fn chapter_title(name: Option<String>, number: f64) -> Option<String> {
    let name = name?.trim().to_string();
    let plain = format!("{number}");
    if name.is_empty() || name == plain || name.parse::<f64>().ok() == Some(number) {
        None
    } else {
        Some(name)
    }
}

impl MangaBall {
    pub fn new() -> Self {
        Self
    }

    pub fn metadata() -> ExtensionMetadata {
        ExtensionMetadata {
            id: "mangaball".to_string(),
            name: "Manga Ball".to_string(),
            version: ext_version!("1.0.0"),
            base_url: BASE_URL.to_string(),
            language: LANGUAGE.to_string(),
            nsfw: false,
            unrestricted_http: false,
            mihon_source_id: Some(1448906013733277368_i64),
            languages: vec![LANGUAGE.to_string()],
            description: Some(
                "Manga Ball (mangaball.com): an aggregator carrying most popular manga, manhwa \
                 and manhua, collected from many other sites. English chapters only."
                    .to_string(),
            ),
            ..Default::default()
        }
    }

    fn search(
        &self,
        page: i32,
        page_size: i32,
        keyword: Option<&str>,
        sort: Option<&str>,
        filters: &[ActiveFilter],
    ) -> ExtensionResult<MangaList> {
        let limit = page_size.clamp(1, MAX_PAGE_SIZE);
        let groups = FilterGroups::from(filters);
        let adult_mode = if prefs::get_bool("show_adult") {
            "all"
        } else {
            "no_18"
        };

        let mut req = HttpRequest::get(format!("{BASE_URL}/api/v1/title/search-advanced"))
            .query("page", page.max(1))
            .query("limit", limit)
            .query("tag_mode", "AND")
            .query("adult_mode", adult_mode);

        let chosen = sort
            .or_else(|| groups.selection_value("sort").filter(|s| !s.is_empty()))
            .or(if keyword.is_some() {
                None
            } else {
                Some("lastupdate")
            });
        if let Some(sort_by) = chosen {
            let order = if sort_by == "name" { "asc" } else { "desc" };
            req = req.query("sort_by", sort_by).query("sort_order", order);
        }
        if let Some(keyword) = keyword {
            req = req.query("keyword", keyword);
        }
        for (group, param) in [
            ("type", "type"),
            ("demographic", "publicationDemographic"),
            ("status", "status"),
        ] {
            if let Some(value) = groups.selection_value(group).filter(|v| !v.is_empty()) {
                req = req.query(param, value);
            }
        }

        let bp = BlueprintBuilder::new("/data")
            .request(req)
            .field("id", Expr::self_ref().ptr("/slug").str_val())
            .field("title", Expr::self_ref().ptr("/name").str_val())
            .field_opt("cover_url", cover_expr(Expr::self_ref().ptr("/image")))
            .scalar_opt("page", Expr::json_root("/pagination/page").int_val())
            .scalar_opt(
                "total_pages",
                Expr::json_root("/pagination/total_pages").int_val(),
            )
            .build();
        let rows = extract::json(None, &bp)?;

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
        let total_pages = rows.get_scalar_i64("total_pages").map(|t| t.max(0) as u32);
        let current = rows.get_scalar_i64("page").unwrap_or(page.max(1) as i64);
        Ok(MangaList {
            manga,
            has_next_page: total_pages.is_some_and(|t| current < t as i64),
            total_pages,
        })
    }

    fn details_request(manga_id: &str) -> HttpRequest {
        HttpRequest::get(format!("{BASE_URL}/api/v1/title/detail/{manga_id}"))
    }

    /// The chapter listing needs the title's database id; Kani stores the slug, as Mihon does.
    fn object_id(&self, manga_id: &str) -> ExtensionResult<String> {
        if is_object_id(manga_id) {
            return Ok(manga_id.to_string());
        }
        let bp = BlueprintBuilder::new("/data")
            .request(Self::details_request(manga_id))
            .field("id", Expr::self_ref().ptr("/_id").str_val())
            .build();
        let rows = extract::json(None, &bp)?;
        rows.rows_get(0)
            .map_err(|_| ExtensionError::parse(format!("no title {manga_id}")))?
            .require_str("/id")
    }

    fn chapter_rows(&self, title_id: &str) -> ExtensionResult<Vec<Value>> {
        let body = kani_shared::serde_json::json!({ "title_id": title_id, "lang": LANGUAGE });
        let req = HttpRequest::post(format!(
            "{BASE_URL}/api/v1/chapter/chapter-listing-by-title-id"
        ))
        .header("Content-Type", "application/json")
        .body(body.to_string().into_bytes());

        match fetch_json(req)? {
            Value::Object(mut map) => match map.remove("data") {
                Some(Value::Array(rows)) => Ok(rows),
                _ => Err(ExtensionError::parse("chapter list has no data".into())),
            },
            _ => Err(ExtensionError::parse(
                "chapter list is not an object".into(),
            )),
        }
    }
}

impl Guest for MangaBall {
    fn get_metadata() -> Result<String, wit_types::ExtensionError> {
        Ok(kani_shared::serde_json::to_string(&MangaBall::metadata())
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

    fn get_chapter_sort_list() -> Result<Vec<wit_types::SortOption>, wit_types::ExtensionError> {
        get_extension()
            .get_chapter_sort_list()
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

    fn get_preferences() -> Result<Vec<PreferenceSpec>, wit_types::ExtensionError> {
        get_extension().get_preferences().map_err(|e| e.into_wit())
    }

    fn get_url(manga_id: String) -> Result<String, wit_types::ExtensionError> {
        get_extension().get_url(&manga_id).map_err(|e| e.into_wit())
    }
}

impl MangaExtension for MangaBall {
    fn name(&self) -> &str {
        "Manga Ball"
    }

    fn get_popular_manga(
        &self,
        page: i32,
        page_size: i32,
        _filters: &[ActiveFilter],
    ) -> ExtensionResult<MangaList> {
        self.search(page, page_size, None, Some("views"), &[])
    }

    fn search_manga(
        &self,
        query: &str,
        page: i32,
        page_size: i32,
        filters: &[ActiveFilter],
    ) -> ExtensionResult<MangaList> {
        let query = query.trim();
        let keyword = (!query.is_empty()).then_some(query);
        self.search(page, page_size, keyword, None, filters)
    }

    fn get_manga_details(&self, manga_id: &str) -> ExtensionResult<MangaInfo> {
        let doc = fetch_json(Self::details_request(manga_id))?;
        let data = doc
            .get("data")
            .filter(|d| d.is_object())
            .ok_or_else(|| ExtensionError::parse(format!("no title {manga_id}")))?;
        let strings = |key: &str| -> Vec<String> {
            data.get(key)
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|v| v.as_str().or_else(|| v.get("name")?.as_str()))
                        .map(|v| v.trim().to_string())
                        .filter(|v| !v.is_empty())
                        .collect()
                })
                .unwrap_or_default()
        };
        let names = |key: &str| -> Vec<String> {
            let mut out: Vec<String> = Vec::new();
            for name in strings(key) {
                if !out.iter().any(|seen| seen.eq_ignore_ascii_case(&name)) {
                    out.push(name);
                }
            }
            out
        };
        let title = data
            .get("name")
            .and_then(Value::as_str)
            .map(String::from)
            .ok_or_else(|| ExtensionError::parse(format!("title {manga_id} has no name")))?;

        let mut description = strings("description").join("\n\n");
        if prefs::get_bool("alt_titles_in_description") {
            let mut seen = vec![title.to_lowercase()];
            let alts: Vec<String> = strings("alternateName")
                .into_iter()
                .filter(|n| {
                    let key = n.trim().to_lowercase();
                    let fresh = !key.is_empty() && !seen.contains(&key);
                    seen.push(key);
                    fresh
                })
                .collect();
            if !alts.is_empty() {
                if !description.is_empty() {
                    description.push_str("\n\n");
                }
                description.push_str("Alternative names:\n");
                description.push_str(&alts.join("\n"));
            }
        }

        Ok(MangaInfo {
            id: manga_id.to_string(),
            title,
            description: (!description.is_empty()).then_some(description),
            status: status_of(data.get("status").and_then(Value::as_str)),
            cover_url: data.get("image").and_then(cover_from),
            authors: names("author"),
            artists: names("artist"),
            tags: names("tags"),
        })
    }

    fn get_chapter_list(
        &self,
        manga_id: &str,
        page: i32,
        _page_size: Option<i32>,
        _sort: Option<String>,
    ) -> ExtensionResult<ChapterList> {
        if page > 1 {
            return Ok(ChapterList {
                chapters: vec![],
                has_next_page: false,
                total_pages: Some(1),
            });
        }

        let title_id = self.object_id(manga_id)?;
        let rows = self.chapter_rows(&title_id)?;
        let text = |row: &Value, key: &str| row.get(key).and_then(Value::as_str).map(String::from);
        let chapters = rows
            .iter()
            .filter(|row| text(row, "lang").is_none_or(|l| l == LANGUAGE))
            .filter_map(|row| {
                let number = row.get("number").and_then(Value::as_f64).unwrap_or(0.0);
                let volume = row
                    .get("volume")
                    .and_then(Value::as_f64)
                    .filter(|v| *v > 0.0)
                    .map(|v| v as i32);
                let scanlator = row
                    .pointer("/group/name")
                    .and_then(Value::as_str)
                    .map(String::from)
                    .or_else(|| text(row, "group_name"));
                Some(ChapterInfo {
                    id: text(row, "id")?,
                    number,
                    title: chapter_title(text(row, "name"), number),
                    volume,
                    scanlator,
                    date_uploaded: text(row, "created_at").as_deref().and_then(parse_timestamp),
                    language: LANGUAGE.to_string(),
                    page_count: None,
                })
            })
            .collect();

        Ok(ChapterList {
            chapters,
            has_next_page: false,
            total_pages: Some(1),
        })
    }

    fn get_pages(&self, _manga_id: &str, chapter_id: &str) -> ExtensionResult<Chapter> {
        let bp = BlueprintBuilder::new("/data/chapter/pages")
            .request(
                HttpRequest::get(format!("{BASE_URL}/api/v1/chapter-detail"))
                    .query("chapter_id", chapter_id),
            )
            .field("index", Expr::index())
            .field("url", Expr::self_ref().str_val())
            .build();

        let rows = extract::json(None, &bp)?;
        let pages: Vec<Page> = rows
            .rows_iter()
            .enumerate()
            .filter_map(|(i, row)| {
                Some(Page {
                    index: row.get_i64("/index").unwrap_or(i as i64) as i32,
                    url: row.require_str("/url").ok()?,
                    transform: None,
                })
            })
            .collect();
        if pages.is_empty() {
            return Err(ExtensionError::unknown("No pages found".into()));
        }
        Ok(Chapter { pages })
    }

    fn get_chapter_sort_list(&self) -> ExtensionResult<Vec<wit_types::SortOption>> {
        Ok(vec![])
    }

    fn get_filter_list(&self) -> ExtensionResult<wit_types::FilterList> {
        Ok(filter_list! {
            "sort", "Sort", Select, [
                ("Best match", ""),
                ("Latest update", "lastupdate"),
                ("Most viewed", "views"),
                ("Top rated", "rating"),
                ("Recently added", "created_at"),
                ("Title (A-Z)", "name"),
            ], default: ("Best match", "");

            "type", "Type", Select, [
                ("All", ""),
                ("Manga", "manga"),
                ("Manhwa", "manhwa"),
                ("Manhua", "manhua"),
                ("Comics", "comics"),
            ], default: ("All", "");

            "demographic", "Demographic", Select, [
                ("All", ""),
                ("Shounen", "shounen"),
                ("Shoujo", "shoujo"),
                ("Seinen", "seinen"),
                ("Josei", "josei"),
            ], default: ("All", "");

            "status", "Status", Select, [
                ("All", ""),
                ("Ongoing", "ongoing"),
                ("Completed", "completed"),
                ("Hiatus", "hiatus"),
                ("Cancelled", "cancelled"),
            ], default: ("All", "");
        })
    }

    fn get_preferences(&self) -> ExtensionResult<Vec<PreferenceSpec>> {
        use kani_shared::preference_list;
        Ok(preference_list! {
            "show_adult", "Show 18+ titles", Toggle,
                default: false,
                description: "Include titles the site marks as 18+ in Popular and search.";

            "alt_titles_in_description", "Alternative titles in description", Toggle,
                default: false,
                description: "Append the title's other names to its description.";
        })
    }

    fn get_url(&self, manga_id: &str) -> ExtensionResult<String> {
        Ok(format!("{BASE_URL}/title-detail/{manga_id}"))
    }
}

use std::sync::OnceLock;
static EXTENSION: OnceLock<MangaBall> = OnceLock::new();
fn get_extension() -> &'static MangaBall {
    EXTENSION.get_or_init(MangaBall::new)
}
bindings::export!(MangaBall);

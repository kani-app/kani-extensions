#[cfg(not(target_family = "wasm"))]
compile_error!(
    "kani-extensions/* are WASM-only -- build with `cargo run -p kani-cli -- build <name>`. \
     If a tool triggered this, it is using --workspace (or defaulting to it); scope it to \
     default-members instead -- clippy/nextest omit the flag, cargo-dist needs precise-builds."
);

use std::collections::HashMap;

use kani_shared::serde_json::Value;

use kani_shared::ast::{BlueprintBuilder, Expr};
use kani_shared::bindings::exports::kani::extension::manga_provider::Guest;
use kani_shared::host_abi::{HttpRequest, JsonHandle, extract, prefs};
use kani_shared::{
    ExtensionError, ExtensionMetadata, ExtensionResult, FilterGroups, MangaExtension, MangaStatus,
    bindings, ext_version, filter_list, to_shared_filters, types::ActiveFilter, wit_types,
};
use wit_types::{
    Chapter, ChapterInfo, ChapterList, MangaInfo, MangaList, MangaListItem, Page, PreferenceSpec,
};

kani_shared::guest_alloc!();

const BASE_URL: &str = "https://atsu.moe";
const IMAGE_BASE: &str = "https://cdn.atsu.moe/static/";
const TYPES: &str = "Manga,Manwha,Manhua,OEL";
const MAX_PAGE_SIZE: i32 = 100;

pub struct Atsumaru;

impl Default for Atsumaru {
    fn default() -> Self {
        Self::new()
    }
}

/// The site serves image paths in three shapes: absolute URLs, `/static/<path>` and a bare
/// `<path>`. All of them resolve under the CDN host, which is what the site itself loads.
fn image_url(path: &str) -> String {
    if path.starts_with("http://") || path.starts_with("https://") {
        return path.to_string();
    }
    if let Some(rest) = path.strip_prefix("//") {
        return format!("https://{rest}");
    }
    let path = path.trim_start_matches('/');
    let path = path.strip_prefix("static/").unwrap_or(path);
    format!("{IMAGE_BASE}{path}")
}

fn status_of(raw: Option<&str>) -> MangaStatus {
    match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("ongoing") => MangaStatus::Ongoing,
        Some("completed") => MangaStatus::Completed,
        Some("hiatus") => MangaStatus::Hiatus,
        Some("canceled") | Some("cancelled") => MangaStatus::Cancelled,
        _ => MangaStatus::Unknown,
    }
}

fn show_adult() -> bool {
    prefs::get_bool("show_adult")
}

/// A chapter id is `<manga id>/<chapter id>`, the shape Mihon stores, so a Mihon backup's
/// chapters match the ones this source lists.
fn split_chapter_id<'a>(manga_id: &'a str, chapter_id: &'a str) -> (&'a str, &'a str) {
    chapter_id.split_once('/').unwrap_or((manga_id, chapter_id))
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

fn page_bounds(page: i32, page_size: i32) -> (i32, i32) {
    let limit = page_size.clamp(1, MAX_PAGE_SIZE);
    (limit, (page.max(1) - 1) * limit)
}

impl Atsumaru {
    pub fn new() -> Self {
        Self
    }

    pub fn metadata() -> ExtensionMetadata {
        ExtensionMetadata {
            id: "atsumaru".to_string(),
            name: "Atsumaru".to_string(),
            version: ext_version!("1.0.0"),
            base_url: BASE_URL.to_string(),
            language: "en".to_string(),
            nsfw: false,
            unrestricted_http: false,
            mihon_source_id: Some(2327480808438768017_i64),
            languages: vec!["en".to_string()],
            description: Some(
                "Atsumaru (atsu.moe): a large English library of manga, manhwa and manhua, with \
                 chapters from several scanlation groups per title."
                    .to_string(),
            ),
            ..Default::default()
        }
    }

    fn manga_rows(
        &self,
        req: HttpRequest,
        container: &str,
        prefix: &str,
    ) -> ExtensionResult<JsonHandle> {
        let ptr = |p: &str| Expr::self_ref().ptr(format!("{prefix}{p}"));
        let bp = BlueprintBuilder::new(container)
            .request(req)
            .field("id", ptr("/id").str_val())
            .field("title", ptr("/title").str_val())
            .field_opt(
                "image",
                ptr("/image")
                    .str_val()
                    .fallback(ptr("/poster/image").str_val())
                    .fallback(ptr("/poster").str_val()),
            )
            .scalar_opt("found", Expr::json_root("/found").int_val())
            .build();
        extract::json(None, &bp)
    }

    fn to_items(rows: &JsonHandle) -> Vec<MangaListItem> {
        rows.rows_iter()
            .filter_map(|row| {
                Some(MangaListItem {
                    id: row.require_str("/id").ok()?,
                    title: row.require_str("/title").ok()?,
                    cover_url: row.get_str("/image").map(|p| image_url(&p)),
                })
            })
            .collect()
    }

    /// Scanlator id to (name, score), from the title page. The chapter list carries only the id.
    fn scanlators(&self, manga_id: &str) -> ExtensionResult<HashMap<String, (String, i64)>> {
        let page = fetch_json(
            HttpRequest::get(format!("{BASE_URL}/api/manga/page")).query("id", manga_id),
        )?;
        let groups = page
            .pointer("/mangaPage/scanlators")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        Ok(groups
            .iter()
            .filter_map(|g| {
                Some((
                    g.get("id")?.as_str()?.to_string(),
                    (
                        g.get("name")?.as_str()?.to_string(),
                        g.get("score").and_then(Value::as_i64).unwrap_or(i64::MIN),
                    ),
                ))
            })
            .collect())
    }
}

impl Guest for Atsumaru {
    fn get_metadata() -> Result<String, wit_types::ExtensionError> {
        Ok(kani_shared::serde_json::to_string(&Atsumaru::metadata())
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

impl MangaExtension for Atsumaru {
    fn name(&self) -> &str {
        "Atsumaru"
    }

    fn get_popular_manga(
        &self,
        page: i32,
        page_size: i32,
        _filters: &[ActiveFilter],
    ) -> ExtensionResult<MangaList> {
        let (limit, offset) = page_bounds(page, page_size);
        let timeframe = prefs::get_str_or("popular_timeframe", "weekly");
        let mut req = HttpRequest::get(format!("{BASE_URL}/api/home2/popular"))
            .query("offset", offset)
            .query("limit", limit)
            .query("types", TYPES)
            .query("mediums", "Comic")
            .query("timeframe", timeframe);
        if show_adult() {
            req = req.query("adult", "1");
        }

        let rows = self.manga_rows(req, "/items", "")?;
        let manga = Self::to_items(&rows);
        Ok(MangaList {
            has_next_page: rows.rows_len() >= limit,
            manga,
            total_pages: None,
        })
    }

    fn search_manga(
        &self,
        query: &str,
        page: i32,
        page_size: i32,
        filters: &[ActiveFilter],
    ) -> ExtensionResult<MangaList> {
        let per_page = page_size.clamp(1, MAX_PAGE_SIZE);
        let query = query.trim();
        let groups = FilterGroups::from(filters);

        let mut filter_by: Vec<String> = ["hidden:!=true", "medium:!=[`Novel`]", "views:>0"]
            .map(String::from)
            .to_vec();
        if !show_adult() {
            filter_by.push("isAdult:=false".to_string());
        }
        for (group, field) in [("type", "type"), ("status", "status")] {
            let values = groups.multiselect_values(group);
            if !values.is_empty() {
                let quoted: Vec<String> = values.iter().map(|v| format!("`{v}`")).collect();
                filter_by.push(format!("{field}:=[{}]", quoted.join(",")));
            }
        }

        let mut req = HttpRequest::get(format!("{BASE_URL}/collections/manga/documents/search"))
            .query("q", if query.is_empty() { "*" } else { query })
            .query("filter_by", filter_by.join(" && "))
            .query("page", page.max(1))
            .query("per_page", per_page);
        if !query.is_empty() {
            req = req
                .query("query_by", "title,englishTitle,otherNames,authors")
                .query("query_by_weights", "4,3,2,1")
                .query("num_typos", "4,3,2,1");
        }
        if let Some(sort) = groups.selection_value("sort").filter(|s| !s.is_empty()) {
            req = req.query("sort_by", sort);
        }

        let rows = self.manga_rows(req, "/hits", "/document")?;
        let manga = Self::to_items(&rows);
        let found = rows.get_scalar_i64("found").unwrap_or(0);
        let total_pages = (found.max(0) as u32).div_ceil(per_page as u32);
        Ok(MangaList {
            manga,
            has_next_page: (page.max(1) as u32) < total_pages,
            total_pages: Some(total_pages),
        })
    }

    fn get_manga_details(&self, manga_id: &str) -> ExtensionResult<MangaInfo> {
        let doc = fetch_json(
            HttpRequest::get(format!("{BASE_URL}/api/manga/page")).query("id", manga_id),
        )?;
        let page = doc
            .get("mangaPage")
            .filter(|p| p.is_object())
            .ok_or_else(|| ExtensionError::parse(format!("no title {manga_id}")))?;
        let text = |v: &Value, key: &str| {
            v.get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(String::from)
        };
        let list = |key: &str| {
            page.get(key)
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default()
        };
        let title = text(page, "title")
            .ok_or_else(|| ExtensionError::parse(format!("title {manga_id} has no name")))?;

        let mut description = text(page, "synopsis").unwrap_or_default();
        if prefs::get_bool("alt_titles_in_description") {
            let mut seen = vec![title.to_lowercase()];
            let alts: Vec<String> = list("otherNames")
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|n| {
                    let key = n.to_lowercase();
                    let fresh = !key.is_empty() && !seen.contains(&key);
                    seen.push(key);
                    fresh
                })
                .map(String::from)
                .collect();
            if !alts.is_empty() {
                if !description.is_empty() {
                    description.push_str("\n\n");
                }
                description.push_str("Alternative names:\n");
                description.push_str(&alts.join("\n"));
            }
        }

        let (mut authors, mut artists) = (Vec::new(), Vec::new());
        for person in list("authors") {
            let Some(name) = text(person, "name").or_else(|| person.as_str().map(String::from))
            else {
                continue;
            };
            if person.get("type").and_then(Value::as_str) == Some("Artist") {
                artists.push(name);
            } else {
                authors.push(name);
            }
        }

        let mut tags: Vec<String> = text(page, "type")
            .map(|t| {
                if t == "Manwha" {
                    "Manhwa".to_string()
                } else {
                    t
                }
            })
            .into_iter()
            .collect();
        tags.extend(list("genres").iter().filter_map(|g| text(g, "name")));

        let poster = page
            .pointer("/poster/image")
            .and_then(Value::as_str)
            .or_else(|| page.get("poster").and_then(Value::as_str));

        Ok(MangaInfo {
            id: manga_id.to_string(),
            title,
            description: (!description.is_empty()).then_some(description),
            status: status_of(page.get("status").and_then(Value::as_str)),
            cover_url: poster.map(image_url),
            authors,
            artists,
            tags,
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

        let groups = self.scanlators(manga_id)?;

        let all = fetch_json(
            HttpRequest::get(format!("{BASE_URL}/api/manga/allChapters"))
                .query("mangaId", manga_id),
        )?;
        let rows = all
            .get("chapters")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();

        struct Entry {
            info: ChapterInfo,
            score: i64,
            created: i64,
        }

        let mut entries: Vec<Entry> = rows
            .iter()
            .filter_map(|row| {
                let chapter = row.get("id")?.as_str()?;
                let group = row
                    .get("scanlationMangaId")
                    .and_then(Value::as_str)
                    .and_then(|g| groups.get(g));
                let created = row.get("createdAt").and_then(Value::as_i64).unwrap_or(0);
                Some(Entry {
                    info: ChapterInfo {
                        id: format!("{manga_id}/{chapter}"),
                        number: row.get("number").and_then(Value::as_f64).unwrap_or(0.0),
                        title: row.get("title").and_then(Value::as_str).map(String::from),
                        volume: None,
                        scanlator: group.map(|(name, _)| name.clone()),
                        date_uploaded: (created > 0).then_some(created / 1000),
                        language: "en".to_string(),
                        page_count: row
                            .get("pageCount")
                            .and_then(Value::as_u64)
                            .map(|v| v.min(u32::MAX as u64) as u32),
                    },
                    score: group.map(|(_, score)| *score).unwrap_or(i64::MIN),
                    created,
                })
            })
            .collect();

        if prefs::get_bool("deduplicate_chapters") {
            let mut best: HashMap<u64, usize> = HashMap::new();
            for (i, e) in entries.iter().enumerate() {
                let key = e.info.number.to_bits();
                match best.get(&key) {
                    Some(&j) if (entries[j].score, entries[j].created) >= (e.score, e.created) => {}
                    _ => {
                        best.insert(key, i);
                    }
                }
            }
            let mut keep: Vec<usize> = best.into_values().collect();
            keep.sort_unstable();
            let mut taken: Vec<Option<Entry>> = entries.into_iter().map(Some).collect();
            entries = keep.into_iter().filter_map(|i| taken[i].take()).collect();
        }

        entries.sort_by(|a, b| {
            b.info
                .number
                .total_cmp(&a.info.number)
                .then_with(|| a.info.scanlator.cmp(&b.info.scanlator))
                .then_with(|| b.created.cmp(&a.created))
        });

        Ok(ChapterList {
            chapters: entries.into_iter().map(|e| e.info).collect(),
            has_next_page: false,
            total_pages: Some(1),
        })
    }

    fn get_pages(&self, manga_id: &str, chapter_id: &str) -> ExtensionResult<Chapter> {
        let (manga, chapter) = split_chapter_id(manga_id, chapter_id);
        let bp = BlueprintBuilder::new("/readChapter/pages")
            .request(
                HttpRequest::get(format!("{BASE_URL}/api/read/chapter"))
                    .query("mangaId", manga)
                    .query("chapterId", chapter),
            )
            .field("index", Expr::index())
            .field("image", Expr::self_ref().ptr("/image").str_val())
            .build();

        let rows = extract::json(None, &bp)?;
        let pages: Vec<Page> = rows
            .rows_iter()
            .enumerate()
            .filter_map(|(i, row)| {
                Some(Page {
                    index: row.get_i64("/index").unwrap_or(i as i64) as i32,
                    url: image_url(&row.require_str("/image").ok()?),
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
                ("Most viewed", "views:desc"),
                ("Trending", "trending:desc"),
                ("Recently added", "dateAdded:desc"),
                ("Newest release", "released:desc"),
                ("Top rated", "mbRating:desc"),
                ("Title (A-Z)", "title:asc"),
            ], default: ("Best match", "");

            "type", "Type", Multiselect, [
                ("Manga", "Manga"),
                ("Manhwa", "Manwha"),
                ("Manhua", "Manhua"),
                ("OEL", "OEL"),
                ("Other", "Other"),
            ];

            "status", "Status", Multiselect, [
                ("Ongoing", "Ongoing"),
                ("Completed", "Completed"),
                ("Hiatus", "Hiatus"),
                ("Cancelled", "Canceled"),
            ];
        })
    }

    fn get_preferences(&self) -> ExtensionResult<Vec<PreferenceSpec>> {
        use kani_shared::preference_list;
        Ok(preference_list! {
            "popular_timeframe", "Popular over", Select,
                [("Today", "daily"), ("This week", "weekly"), ("This month", "monthly"), ("All time", "all")],
                default: "weekly",
                description: "The period the Popular list ranks titles over.";

            "show_adult", "Show adult titles", Toggle,
                default: false,
                description: "Include titles the site marks as adult in Popular and search.";

            "deduplicate_chapters", "One upload per chapter", Toggle,
                default: false,
                description: "List each chapter once, from the highest-rated group that has it, instead of every group's upload.";

            "alt_titles_in_description", "Alternative titles in description", Toggle,
                default: false,
                description: "Append the title's other names to its description.";
        })
    }

    fn get_url(&self, manga_id: &str) -> ExtensionResult<String> {
        Ok(format!("{BASE_URL}/manga/{manga_id}"))
    }
}

use std::sync::OnceLock;
static EXTENSION: OnceLock<Atsumaru> = OnceLock::new();
fn get_extension() -> &'static Atsumaru {
    EXTENSION.get_or_init(Atsumaru::new)
}
bindings::export!(Atsumaru);

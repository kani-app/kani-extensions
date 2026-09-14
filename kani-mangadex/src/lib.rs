#[cfg(not(target_family = "wasm"))]
compile_error!(
    "kani-extensions/* are WASM-only -- build with `cargo run -p kani-cli -- build <name>`. \
     If a tool triggered this, it is using --workspace (or defaulting to it); scope it to \
     default-members instead -- clippy/nextest omit the flag, cargo-dist needs precise-builds."
);

use kani_shared::ast::{BlueprintBuilder, Expr};
use kani_shared::bindings::exports::kani::extension::manga_provider::Guest;
use kani_shared::host_abi::{HttpRequest, extract, prefs};
use kani_shared::{
    ApplyFilters, ArrayFormat, ExtensionError, ExtensionMetadata, ExtensionResult, FilterGroups,
    MangaExtension, MangaStatus, bindings, chapter_sort_list, ext_version, to_shared_filters,
    types::ActiveFilter, wit_types,
};
use wit_types::{
    Chapter, ChapterInfo, ChapterList, MangaInfo, MangaList, MangaListItem, Page, PreferenceSpec,
    SortOption,
};

kani_shared::guest_alloc!();

const LANGS: [&str; 7] = ["en", "ja-ro", "ja", "ko", "zh", "fr", "es"];

static TAG_CACHE: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();

pub struct MangaDex {
    base_url: String,
}

impl Default for MangaDex {
    fn default() -> Self {
        Self::new()
    }
}

impl MangaDex {
    pub fn new() -> Self {
        Self {
            base_url: "https://api.mangadex.org".to_string(),
        }
    }

    pub fn metadata() -> ExtensionMetadata {
        ExtensionMetadata {
            id: "mangadex".to_string(),
            name: "MangaDex".to_string(),
            version: ext_version!("0.1.1"),
            base_url: "https://api.mangadex.org".to_string(),
            language: "multi".to_string(),
            nsfw: false,
            unrestricted_http: false,
            mihon_source_id: Some(2499283573021220255_i64),
            rate_limit: None,
            ..Default::default()
        }
    }

    fn fetch_manga_list(
        &self,
        req: HttpRequest,
        limit: i32,
        page_size: i32,
        offset: i32,
    ) -> ExtensionResult<MangaList> {
        let lang_keys = LANGS.map(Expr::lit);

        if offset >= 10_000 {
            return Ok(MangaList {
                manga: vec![],
                has_next_page: false,
                total_pages: None,
            });
        }

        let bp = BlueprintBuilder::new("/data")
            .request(req)
            .field("id", Expr::self_ref().ptr("/id").str_val())
            .field(
                "title",
                Expr::json_array(vec![
                    Expr::self_ref().ptr("/attributes/altTitles").json_fold(),
                    Expr::self_ref().ptr("/attributes/title"),
                ])
                .filter(Expr::var("$item").ne(Expr::null()))
                .json_fold()
                .coalesce_keys(lang_keys.clone())
                .fallback_str("Unknown Title"),
            )
            .field_opt(
                "cover_url",
                Expr::let_bind(
                    "$id",
                    Expr::self_ref().ptr("/id").str_val(),
                    Expr::let_bind(
                        "$filename",
                        Expr::self_ref()
                            .ptr("/relationships")
                            .find_kv("type", "cover_art")
                            .ptr("/attributes/fileName")
                            .str_val(),
                        Expr::if_then_else(
                            Expr::var("$filename").ne(Expr::null()),
                            Expr::format(
                                "https://uploads.mangadex.org/covers/{}/{}{}",
                                vec![
                                    Expr::var("$id"),
                                    Expr::var("$filename"),
                                    Expr::pref("cover_size").fallback_str(".512.jpg"),
                                ],
                            ),
                            Expr::null(),
                        ),
                    ),
                ),
            )
            .scalar(
                "has_next_page",
                Expr::json_root("/offset")
                    .int_val()
                    .fallback(Expr::num(0.0))
                    .add(Expr::json_root("/limit").int_val().fallback(Expr::num(0.0)))
                    .lt(Expr::json_root("/total").int_val().fallback(Expr::num(0.0))),
            )
            .scalar_opt("total_pages", Expr::json_root("/total").int_val())
            .build();

        let rows = extract::json(None, &bp)?;

        let has_next_page = rows.get_scalar_bool("has_next_page") && (offset + limit) < 10_000;
        let total_pages = rows.get_scalar_i64("total_pages").map(|v| v.min(10_000));

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
            total_pages: total_pages.map(|total| (total as u32).div_ceil(page_size as u32)),
        })
    }

    fn get_or_fetch_tags(&self) -> ExtensionResult<&'static [(String, String)]> {
        if let Some(tags) = TAG_CACHE.get() {
            return Ok(tags.as_slice());
        }

        let bp = BlueprintBuilder::new("/data")
            .request(HttpRequest::get(format!("{}/manga/tag", self.base_url)))
            .field("id", Expr::self_ref().ptr("/id").str_val())
            .field(
                "name",
                Expr::self_ref()
                    .ptr("/attributes/name")
                    .coalesce_keys(["en", "ja-ro", "ja"].map(Expr::lit))
                    .fallback_str("Unknown"),
            )
            .build();

        let rows = extract::json(None, &bp)?;
        let mut tags: Vec<(String, String)> = (0..rows.rows_len())
            .filter_map(|i| {
                let row = rows.rows_get(i).ok()?;
                Some((row.require_str("/name").ok()?, row.require_str("/id").ok()?))
            })
            .collect();
        tags.sort_by(|a, b| a.0.cmp(&b.0));

        let _ = TAG_CACHE.set(tags);
        Ok(TAG_CACHE.get().expect("just set").as_slice())
    }
}

fn apply_content_rating_prefs(mut req: HttpRequest) -> HttpRequest {
    let mut ratings = Vec::new();
    // Safe and Suggestive default to enabled; erotica/pornographic default to off.
    if prefs::get_str_or("cr_safe", "true") != "false" {
        ratings.push("safe");
    }
    if prefs::get_str_or("cr_suggestive", "true") != "false" {
        ratings.push("suggestive");
    }
    if prefs::get_bool("cr_erotica") {
        ratings.push("erotica");
    }
    if prefs::get_bool("cr_pornographic") {
        ratings.push("pornographic");
    }
    if ratings.is_empty() {
        ratings.push("safe");
    }
    for r in ratings {
        req = req.query("contentRating[]", r);
    }
    req
}

impl Guest for MangaDex {
    fn get_metadata() -> Result<String, wit_types::ExtensionError> {
        Ok(kani_shared::serde_json::to_string(&MangaDex::metadata())
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
    fn get_chapter_sort_list() -> Result<Vec<SortOption>, wit_types::ExtensionError> {
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

impl MangaExtension for MangaDex {
    fn name(&self) -> &str {
        "MangaDex"
    }

    fn get_popular_manga(
        &self,
        page: i32,
        page_size: i32,
        _filters: &[ActiveFilter],
    ) -> ExtensionResult<MangaList> {
        let offset = (page - 1) * page_size;
        // MangaDex rejects requests where offset + limit > 10,000. Cap the limit so the
        // last partial page is fetched cleanly and has_next_page resolves to false.
        let limit = page_size.min(10_000 - offset);
        let mut req = HttpRequest::get(format!("{}/manga", self.base_url))
            .query("limit", limit.to_string())
            .query("offset", offset.to_string())
            .query("includes[]", "cover_art")
            .query("includes[]", "author")
            .query("includes[]", "artist")
            .query("order[followedCount]", "desc");

        req = apply_content_rating_prefs(req);

        let lang = prefs::get_str_or("translated_language", "en");
        req = req.query("availableTranslatedLanguage[]", lang);

        self.fetch_manga_list(req, limit, page_size, offset)
    }

    fn search_manga(
        &self,
        query: &str,
        page: i32,
        page_size: i32,
        filters: &[ActiveFilter],
    ) -> ExtensionResult<MangaList> {
        let offset = (page - 1) * page_size;
        // MangaDex rejects requests where offset + limit > 10,000.
        let limit = page_size.min(10_000 - offset);
        let mut req = HttpRequest::get(format!("{}/manga", self.base_url))
            .query("limit", limit.to_string())
            .query("offset", offset.to_string())
            .query("includes[]", "cover_art")
            .query("includes[]", "author")
            .query("includes[]", "artist");

        if !query.is_empty() {
            req = req.query("title", query);
        }

        let mapping: &[(&str, &str)] = &[
            ("content_rating", "contentRating"),
            ("status", "status"),
            ("demographic", "publicationDemographic"),
            ("original_lang", "originalLanguage"),
            ("tags_include", "includedTags"),
            ("tags_exclude", "excludedTags"),
            ("incl_tags_mode", "includedTagsMode"),
            ("excl_tags_mode", "excludedTagsMode"),
            ("has_chapters", "hasAvailableChapters"),
        ];
        req = req.apply_filters_mapped_fmt(filters, mapping, ArrayFormat::Bracket);

        let fg = FilterGroups::from(filters);

        // Fall back to content rating preferences when no filter explicitly sets it.
        if !fg.has_any("content_rating") {
            req = apply_content_rating_prefs(req);
        }

        // order[field]=desc uses a non-standard key shape, handled outside the mapper.
        if let Some(order_val) = fg.selection_value("order_by") {
            req = req.query(format!("order[{}]", order_val), "desc");
        }

        let lang = prefs::get_str_or("translated_language", "en");
        req = req.query("availableTranslatedLanguage[]", lang);

        self.fetch_manga_list(req, limit, page_size, offset)
    }

    fn get_manga_details(&self, manga_id: &str) -> ExtensionResult<MangaInfo> {
        let req = HttpRequest::get(format!("{}/manga/{}", self.base_url, manga_id))
            .query("includes[]", "artist")
            .query("includes[]", "author")
            .query("includes[]", "cover_art");

        let lang_keys_full = LANGS.map(Expr::lit);
        let lang_keys_short = ["en", "ja-ro", "ja"].map(Expr::lit);

        let bp = BlueprintBuilder::new("")
            .request(req)
            .field(
                "title",
                Expr::json_array(vec![
                    Expr::self_ref()
                        .ptr("/data/attributes/altTitles")
                        .json_fold(),
                    Expr::self_ref().ptr("/data/attributes/title"),
                ])
                .filter(Expr::var("$item").ne(Expr::null()))
                .json_fold()
                .coalesce_keys(lang_keys_full)
                .fallback_str("Unknown Title"),
            )
            .field_opt(
                "description",
                Expr::json_root("/data/attributes/description").coalesce_keys(lang_keys_short),
            )
            .field(
                "status",
                Expr::json_root("/data/attributes/status")
                    .str_val()
                    .lookup(vec![
                        ("ongoing", "ongoing"),
                        ("completed", "completed"),
                        ("hiatus", "hiatus"),
                        ("cancelled", "cancelled"),
                    ])
                    .fallback_str("unknown"),
            )
            .field_opt(
                "cover_url",
                Expr::let_bind(
                    "$filename",
                    Expr::json_root("/data/relationships")
                        .find_kv("type", "cover_art")
                        .ptr("/attributes/fileName")
                        .str_val(),
                    Expr::if_then_else(
                        Expr::var("$filename").ne(Expr::null()),
                        Expr::format(
                            "https://uploads.mangadex.org/covers/{}/{}{}",
                            vec![
                                Expr::lit(manga_id),
                                Expr::var("$filename"),
                                Expr::pref("cover_size").fallback_str(".512.jpg"),
                            ],
                        ),
                        Expr::null(),
                    ),
                ),
            )
            .field(
                "authors",
                Expr::json_root("/data/relationships")
                    .filter(
                        Expr::var("$item")
                            .ptr("/type")
                            .str_val()
                            .eq(Expr::lit("author")),
                    )
                    .map(Expr::var("$item").ptr("/attributes/name").str_val()),
            )
            .field(
                "artists",
                Expr::json_root("/data/relationships")
                    .filter(
                        Expr::var("$item")
                            .ptr("/type")
                            .str_val()
                            .eq(Expr::lit("artist")),
                    )
                    .map(Expr::var("$item").ptr("/attributes/name").str_val()),
            )
            .field(
                "tags",
                Expr::json_root("/data/attributes/tags").map(
                    Expr::var("$item")
                        .ptr("/attributes/name")
                        .coalesce_keys(["en", "ja-ro", "ja"].map(Expr::lit)),
                ),
            )
            .build();

        let rows = extract::json(None, &bp)?;
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
            cover_url: row.get_str("/cover_url"),
            authors: row.get_array_of_strings("/authors"),
            artists: row.get_array_of_strings("/artists"),
            tags: row.get_array_of_strings("/tags"),
        })
    }

    fn get_chapter_sort_list(&self) -> ExtensionResult<Vec<wit_types::SortOption>> {
        Ok(chapter_sort_list! {
            "chapter",   "Chapter";
            "volume",    "Volume";
            "publishAt", "Published";
            "updatedAt", "Last Updated";
            "createdAt", "Created";
        })
    }

    fn get_chapter_list(
        &self,
        manga_id: &str,
        page: i32,
        page_size: Option<i32>,
        sort: Option<String>,
    ) -> ExtensionResult<ChapterList> {
        let limit = page_size.unwrap_or(500).min(500);
        let offset = (page - 1) * limit;

        let (sort_field, order) = sort
            .as_deref()
            .and_then(|s| s.rsplit_once('_'))
            .unwrap_or(("chapter", "desc"));

        let lang = prefs::get_str_or("translated_language", "en");

        let req = HttpRequest::get(format!("{}/manga/{}/feed", self.base_url, manga_id))
            .query("limit", limit.to_string())
            .query("offset", offset.to_string())
            .query("translatedLanguage[]", lang)
            .query(format!("order[{}]", sort_field), order)
            .query("includeFutureUpdates", "0")
            .query("includes[]", "scanlation_group");

        let bp = BlueprintBuilder::new("/data")
            .request(req)
            .field("id", Expr::self_ref().ptr("/id").str_val())
            .field(
                "number",
                Expr::self_ref()
                    .ptr("/attributes/chapter")
                    .str_val()
                    .parse_float()
                    .fallback(Expr::num(0.0)),
            )
            .field_opt("title", Expr::self_ref().ptr("/attributes/title").str_val())
            .field_opt(
                "volume",
                Expr::self_ref()
                    .ptr("/attributes/volume")
                    .str_val()
                    .parse_int(),
            )
            .field_opt(
                "scanlator",
                Expr::self_ref()
                    .ptr("/relationships")
                    .find_kv("type", "scanlation_group")
                    .ptr("/attributes/name")
                    .str_val(),
            )
            .field_opt(
                "date_uploaded",
                Expr::self_ref()
                    .ptr("/attributes/createdAt")
                    .str_val()
                    .date_parse_rfc3339(),
            )
            .field(
                "language",
                Expr::self_ref()
                    .ptr("/attributes/translatedLanguage")
                    .str_val()
                    .fallback_str(""),
            )
            .field_opt(
                "page_count",
                Expr::self_ref().ptr("/attributes/pages").int_val(),
            )
            .scalar(
                "has_next_page",
                Expr::json_root("/offset")
                    .int_val()
                    .fallback(Expr::num(0.0))
                    .add(Expr::json_root("/limit").int_val().fallback(Expr::num(0.0)))
                    .lt(Expr::json_root("/total").int_val().fallback(Expr::num(0.0))),
            )
            .scalar_opt("total", Expr::json_root("/total").int_val())
            .build();

        let rows = extract::json(None, &bp)?;
        let has_next_page = rows.get_scalar_bool("has_next_page");
        let total_pages = rows
            .get_scalar_i64("total")
            .map(|total| (total.clamp(0, 10_000) as u32).div_ceil(limit.max(1) as u32));
        let count = rows.rows_len();

        let chapters = (0..count)
            .filter_map(|i| {
                let row = rows.rows_get(i).ok()?;
                Some(ChapterInfo {
                    id: row.require_str("/id").ok()?,
                    number: row.get_f64("/number").unwrap_or(0.0),
                    title: row.get_str("/title"),
                    volume: row.get_i64("/volume").map(|v| v as i32),
                    scanlator: row.get_str("/scanlator"),
                    date_uploaded: row.get_i64("/date_uploaded"),
                    language: row.get_str("/language").unwrap_or_default(),
                    page_count: row.get_i64("/page_count").map(|v| v as u32),
                })
            })
            .collect();

        Ok(ChapterList {
            chapters,
            has_next_page,
            total_pages,
        })
    }

    fn get_pages(&self, _manga_id: &str, chapter_id: &str) -> ExtensionResult<Chapter> {
        let data_saver = prefs::get_bool("data_saver");
        let (array_path, url_segment) = if data_saver {
            ("/chapter/dataSaver", "data-saver")
        } else {
            ("/chapter/data", "data")
        };

        let bp = BlueprintBuilder::new(array_path)
            .request(HttpRequest::get(format!(
                "{}/at-home/server/{}",
                self.base_url, chapter_id
            )))
            .bind("$base_url", Expr::json_root("/baseUrl").str_val())
            .bind("$hash", Expr::json_root("/chapter/hash").str_val())
            .field("index", Expr::index())
            .field(
                "url",
                Expr::format(
                    "{}/{}/{}/{}",
                    vec![
                        Expr::var("$base_url"),
                        Expr::lit(url_segment),
                        Expr::var("$hash"),
                        Expr::self_ref().str_val(),
                    ],
                ),
            )
            .build();

        let rows = extract::json(None, &bp)?;
        let count = rows.rows_len();
        if count == 0 {
            return Err(ExtensionError::unknown("No pages found".into()));
        }

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

    fn get_filter_list(&self) -> ExtensionResult<wit_types::FilterList> {
        use kani_shared::filter_list;

        let tags = self.get_or_fetch_tags()?;

        let mut list = filter_list! {
            "content_rating", "Content Rating", Multiselect, [
                ("Safe",         "safe"),
                ("Suggestive",   "suggestive"),
                ("Erotica",      "erotica"),
                ("Pornographic", "pornographic"),
            ];

            "status", "Status", Multiselect, [
                ("Ongoing",   "ongoing"),
                ("Completed", "completed"),
                ("Hiatus",    "hiatus"),
                ("Cancelled", "cancelled"),
            ];

            "demographic", "Demographic", Multiselect, [
                ("Shounen", "shounen"),
                ("Shoujo",  "shoujo"),
                ("Josei",   "josei"),
                ("Seinen",  "seinen"),
                ("None",    "none"),
            ];

            "original_lang", "Original Language", Multiselect, [
                ("Japanese",              "ja"),
                ("Korean",                "ko"),
                ("Chinese (Simplified)",  "zh"),
                ("Chinese (Traditional)", "zh-hk"),
                ("English",               "en"),
                ("French",                "fr"),
                ("Spanish (Spain)",       "es"),
                ("Spanish (Latin Am.)",   "es-la"),
                ("German",                "de"),
                ("Portuguese (Brazil)",   "pt-br"),
                ("Russian",               "ru"),
                ("Indonesian",            "id"),
                ("Vietnamese",            "vi"),
                ("Thai",                  "th"),
                ("Arabic",                "ar"),
            ];

            "incl_tags_mode", "Include Tags Mode", Select, [
                ("AND — all tags required", "AND"),
                ("OR — any tag matches",    "OR"),
            ], default: ("AND — all tags required", "AND");

            "excl_tags_mode", "Exclude Tags Mode", Select, [
                ("OR — exclude if any match", "OR"),
                ("AND — exclude if all match", "AND"),
            ], default: ("OR — exclude if any match", "OR");

            "has_chapters:1", "Has Available Chapters", Checkbox;

            "order_by", "Sort By", Sort, [
                ("Latest Chapter",   "latestUploadedChapter"),
                ("Most Followed",    "followedCount"),
                ("Relevance",        "relevance"),
                ("Highest Rated",    "rating"),
                ("Newest",           "createdAt"),
                ("Recently Updated", "updatedAt"),
                ("Title",            "title"),
                ("Year",             "year"),
            ];
        };

        let make_tag_filter = |id: &str, label: &str| wit_types::FilterDef {
            id: id.to_string(),
            name: label.to_string(),
            tag: wit_types::FilterTypeTag::Multiselect,
            options: tags
                .iter()
                .map(|(name, uuid)| wit_types::FilterOption {
                    filter_name: id.to_string(),
                    name: name.clone(),
                    value: uuid.clone(),
                })
                .collect(),
            default_value: None,
            semantic: Some(wit_types::FilterSemantic::Tag),
        };

        list.filters
            .push(make_tag_filter("tags_include", "Include Tags"));
        list.filters
            .push(make_tag_filter("tags_exclude", "Exclude Tags"));

        Ok(list)
    }

    fn get_preferences(&self) -> ExtensionResult<Vec<PreferenceSpec>> {
        use kani_shared::preference_list;
        Ok(preference_list! {
            "cover_size", "Cover Size", Select,
                [("Small", ".256.jpg"), ("Medium", ".512.jpg"), ("Full", "")],
                default: ".512.jpg",
                description: "Size of manga cover thumbnails.";

            "translated_language", "Chapter Language", Select,
                [
                    ("English",               "en"),
                    ("Japanese",              "ja"),
                    ("Japanese (Romanized)",  "ja-ro"),
                    ("Korean",                "ko"),
                    ("Chinese (Simplified)",  "zh"),
                    ("Chinese (Traditional)", "zh-hk"),
                    ("French",                "fr"),
                    ("Spanish (Spain)",       "es"),
                    ("Spanish (Latin Am.)",   "es-la"),
                    ("German",                "de"),
                    ("Portuguese (Brazil)",   "pt-br"),
                    ("Russian",               "ru"),
                    ("Indonesian",            "id"),
                    ("Vietnamese",            "vi"),
                    ("Thai",                  "th"),
                ],
                default: "en",
                description: "Language for chapter listings and search availability filter.";

            "cr_safe", "Show Safe content", Toggle,
                default: true,
                description: "Include safe-rated manga in popular and search listings.";

            "cr_suggestive", "Show Suggestive content", Toggle,
                default: true,
                description: "Include suggestive-rated manga in popular and search listings.";

            "cr_erotica", "Show Erotica content", Toggle,
                default: false,
                description: "Include erotica-rated manga in popular and search listings.";

            "cr_pornographic", "Show Pornographic content", Toggle,
                default: false,
                description: "Include pornographic-rated manga in popular and search listings.";

            "data_saver", "Data Saver Mode", Toggle,
                default: false,
                description: "Load compressed page images to reduce bandwidth usage.";
        })
    }

    fn get_url(&self, manga_id: &str) -> ExtensionResult<String> {
        Ok(format!("https://mangadex.org/title/{}", manga_id))
    }
}

use std::sync::OnceLock;
static EXTENSION: OnceLock<MangaDex> = OnceLock::new();
fn get_extension() -> &'static MangaDex {
    EXTENSION.get_or_init(MangaDex::new)
}
bindings::export!(MangaDex);

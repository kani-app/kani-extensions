#[cfg(not(target_family = "wasm"))]
compile_error!(
    "kani-extensions/* are WASM-only -- build with `cargo run -p kani-cli -- build <name>`. \
     If a tool triggered this, it is using --workspace (or defaulting to it); scope it to \
     default-members instead -- clippy/nextest omit the flag, cargo-dist needs precise-builds."
);

use kani_shared::FilterGroups;
use kani_shared::ast::{BlueprintBuilder, Expr};
use kani_shared::bindings::exports::kani::extension::manga_provider::Guest;
use kani_shared::host_abi::{HttpRequest, JsonHandle, extract, prefs, v8_context};
use kani_shared::{
    ExtensionError, ExtensionMetadata, ExtensionResult, MangaExtension, MangaStatus, bindings,
    decode_manga_id, encode_manga_id, to_shared_filters, types::ActiveFilter, wit_types,
};
use kani_shared::{chapter_sort_list, ext_version};
use wit_types::{
    Chapter, ChapterInfo, ChapterList, FilterDef, FilterList, FilterOption, FilterTypeTag,
    MangaInfo, MangaList, MangaListItem, Page, PrefKind, PreferenceSpec,
};

kani_shared::guest_alloc!();

// ── Preference keys ──────────────────────────────────────────────────────────

const PREF_POSTER_QUALITY: &str = "poster_quality";
const PREF_CONTENT_RATING: &str = "content_rating";
const PREF_BLOCKED_GENRES: &str = "blocked_genres";

// ── Option slices (shared between filter list and preferences) ───────────────

const GENRES: &[(&str, &str)] = &[
    ("Romance", "23"),
    ("Drama", "11"),
    ("Comedy", "9"),
    ("Fantasy", "12"),
    ("Slice of Life", "25"),
    ("Action", "6"),
    ("Boys Love", "8"),
    ("Adventure", "7"),
    ("Adult", "87264"),
    ("Smut", "87268"),
    ("Psychological", "22"),
    ("Mystery", "20"),
    ("Historical", "14"),
    ("Mature", "87267"),
    ("Tragedy", "29"),
    ("Sci-Fi", "24"),
    ("Ecchi", "87265"),
    ("Horror", "15"),
    ("Girls Love", "13"),
    ("Isekai", "16"),
    ("Hentai", "87266"),
    ("Thriller", "28"),
    ("Sports", "26"),
    ("Crime", "10"),
    ("Philosophical", "21"),
    ("Mecha", "18"),
    ("Wuxia", "30"),
    ("Medical", "19"),
    ("Superhero", "27"),
    ("Magical Girls", "17"),
];

const FORMATS: &[(&str, &str)] = &[
    ("4-Koma", "93164"),
    ("Adaptation", "93167"),
    ("Anthology", "93165"),
    ("Award Winning", "93166"),
    ("Doujinshi", "93168"),
    ("Full Color", "93172"),
    ("Long Strip", "93170"),
    ("Oneshot", "93169"),
    ("Web Comic", "93171"),
];

const YEAR_MIN: u16 = 1928;
const YEAR_MAX: u16 = 2030;

// ── ID helpers ───────────────────────────────────────────────────────────────

// IDs are encoded as base64url("{api_id}|{page_slug}").
// The API ID (hid for manga, numeric for chapters) is used in REST calls.
// The page slug (e.g. "9kl6-billy-bat") is required for browser page URLs.

fn id_parts(encoded: &str) -> (String, String) {
    let raw = decode_manga_id(encoded);
    if let Some((id, slug)) = raw.split_once('|') {
        (id.to_string(), slug.to_string())
    } else {
        (raw.clone(), raw)
    }
}

/// ASCII-only URL slug: lowercase, non-alphanumeric runs → single `-`.
fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = true;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    if out.ends_with('-') {
        out.pop();
    }
    out
}

// ── Extension struct ─────────────────────────────────────────────────────────

pub struct Comix;

impl Default for Comix {
    fn default() -> Self {
        Self
    }
}

impl Comix {
    pub fn metadata() -> ExtensionMetadata {
        ExtensionMetadata {
            id: "comix".to_string(),
            name: "Comix".to_string(),
            version: ext_version!("0.5.2"),
            base_url: "https://comix.to".to_string(),
            language: "multi".to_string(),
            nsfw: false,
            unrestricted_http: false,
            mihon_source_id: None,
            rate_limit: None,
            ..Default::default()
        }
    }

    // ── Preference helpers ────────────────────────────────────────────────────

    fn poster_quality() -> String {
        prefs::get_str_or(PREF_POSTER_QUALITY, "large")
    }

    fn content_rating() -> String {
        prefs::get_str_or(PREF_CONTENT_RATING, "suggestive")
    }

    fn blocked_genres() -> Vec<String> {
        prefs::get_list(PREF_BLOCKED_GENRES)
    }

    // ── Tag search ────────────────────────────────────────────────────────────

    fn tag_search(&self, q: &str, kind: &str) -> ExtensionResult<Vec<String>> {
        let resp = HttpRequest::get("https://comix.to/api/v1/tags/search")
            .query("q", q)
            .query("type", kind)
            .send()?;
        let handle = JsonHandle::parse(&resp.body)?;
        let path = if handle.array_len("/result").is_some() {
            "/result"
        } else {
            ""
        };
        Ok(handle
            .array_iter(path)
            .filter_map(|item| item.get_i64("/id").map(|id| id.to_string()))
            .collect())
    }

    // ── Request building ──────────────────────────────────────────────────────

    fn build_manga_request(
        &self,
        query: Option<&str>,
        page: i32,
        page_size: i32,
        filters: &[ActiveFilter],
    ) -> ExtensionResult<HttpRequest> {
        let fg = FilterGroups::from(filters);

        let mut req = HttpRequest::get("https://comix.to/api/v1/manga")
            .query("limit", page_size.to_string())
            .query("page", page.to_string());

        let has_query = query.map(|q| !q.is_empty()).unwrap_or(false);
        if let Some(q) = query.filter(|q| !q.is_empty()) {
            req = req.query("keyword", q);
        }

        // Sort (filter overrides default; default differs for search vs popular)
        if let Some(sort_val) = fg.selection_value("sort") {
            if let Some((key, dir)) = sort_val.rsplit_once('_') {
                req = req.query(key, dir);
            }
        } else if has_query {
            req = req.query("order[relevance]", "desc");
        } else {
            req = req.query("order[score]", "desc");
        }

        // Content rating (filter > pref)
        let rating = fg
            .selection_value("content_rating")
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(Self::content_rating);
        if !rating.is_empty() {
            req = req.query("content_rating", &rating);
        }

        if let Some(status) = fg.selection_value("status").filter(|s| !s.is_empty()) {
            req = req.query("status", status);
        }

        for t in fg.multiselect_values("type") {
            req = req.query("types[]", t);
        }
        for d in fg.multiselect_values("demographics") {
            req = req.query("demographics[]", d);
        }

        // Genres + formats share the same API include/exclude params
        let mut genres_in: Vec<String> = fg
            .multiselect_values("genres_include")
            .iter()
            .map(|s| s.to_string())
            .collect();
        genres_in.extend(
            fg.multiselect_values("formats_include")
                .iter()
                .map(|s| s.to_string()),
        );

        let mut genres_ex: Vec<String> = fg
            .multiselect_values("genres_exclude")
            .iter()
            .map(|s| s.to_string())
            .collect();
        genres_ex.extend(
            fg.multiselect_values("formats_exclude")
                .iter()
                .map(|s| s.to_string()),
        );

        // Blocked genres (always applied unless explicitly included)
        for g in Self::blocked_genres() {
            if !genres_in.contains(&g) && !genres_ex.contains(&g) {
                genres_ex.push(g);
            }
        }

        for g in &genres_in {
            req = req.query("genres_in[]", g.as_str());
        }
        for g in &genres_ex {
            req = req.query("genres_ex[]", g.as_str());
        }

        if (!genres_in.is_empty() || !genres_ex.is_empty())
            && let Some(mode) = fg.selection_value("genres_mode")
        {
            req = req.query("genres_mode", mode);
        }

        if let Some(min) = fg.text_value("min_chapters") {
            req = req.query("minimum_chapters", min);
        }
        if let Some(tags) = fg.text_value("tags") {
            req = req.query("tags[]", tags);
        }
        if let Some(yr) = fg.selection_value("year_from").filter(|s| !s.is_empty()) {
            req = req.query("year_from", yr);
        }
        if let Some(yr) = fg.selection_value("year_to").filter(|s| !s.is_empty()) {
            req = req.query("year_to", yr);
        }

        // Author / Artist resolved to tag IDs
        if let Some(author) = fg.text_value("author") {
            for id in self.tag_search(author, "author")? {
                req = req.query("authors[]", &id);
            }
        }
        if let Some(artist) = fg.text_value("artist") {
            for id in self.tag_search(artist, "artist")? {
                req = req.query("artists[]", &id);
            }
        }

        Ok(req)
    }

    // ── Manga list helper ─────────────────────────────────────────────────────

    fn fetch_manga_list(&self, req: HttpRequest) -> ExtensionResult<MangaList> {
        let quality = Self::poster_quality();
        let poster_field = format!("/poster/{}", quality);

        let bp = BlueprintBuilder::new("/result/items")
            .request(req)
            .field("id", Expr::self_ref().ptr("/hid").str_val())
            .field("title", Expr::self_ref().ptr("/title").str_val())
            .field_opt("slug", Expr::self_ref().ptr("/slug").str_val())
            .field_opt("cover", Expr::self_ref().ptr(&poster_field).str_val())
            .scalar_opt(
                "last_page",
                Expr::json_root("/result/meta/lastPage").int_val(),
            )
            .scalar_opt(
                "has_next",
                Expr::json_root("/result/meta/hasNext").bool_val(),
            )
            .build();

        let rows = extract::json(None, &bp)?;
        let last_page = rows.get_scalar_i64("last_page").unwrap_or(0);
        let has_next = rows.get_scalar_bool("has_next");

        let manga = rows
            .rows_iter()
            .filter_map(|row| {
                let hid = row.require_str("/id").ok()?;
                let title = row.require_str("/title").ok()?;
                // Prefer the API's `slug` field; fall back to constructing from hid + title.
                let page_slug = row
                    .get_str("/slug")
                    .unwrap_or_else(|| format!("{}-{}", hid, slugify(&title)));
                Some(MangaListItem {
                    id: encode_manga_id(&format!("{}|{}", hid, page_slug)),
                    title,
                    cover_url: row.get_str("/cover"),
                })
            })
            .collect();

        Ok(MangaList {
            manga,
            has_next_page: has_next,
            total_pages: Some(last_page as u32),
        })
    }
}

// ── WIT Guest (thin delegation) ──────────────────────────────────────────────

impl Guest for Comix {
    fn get_metadata() -> Result<String, wit_types::ExtensionError> {
        Ok(kani_shared::serde_json::to_string(&Comix::metadata())
            .expect("ExtensionMetadata serializes to JSON"))
    }

    fn get_popular_manga(
        page: i32,
        page_size: i32,
        filters: Vec<wit_types::ActiveFilter>,
    ) -> Result<MangaList, wit_types::ExtensionError> {
        let shared = to_shared_filters(filters);
        get_ext()
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
        get_ext()
            .search_manga(&query, page, page_size, &shared)
            .map_err(|e| e.into_wit())
    }

    fn get_filter_list() -> Result<FilterList, wit_types::ExtensionError> {
        get_ext().get_filter_list().map_err(|e| e.into_wit())
    }

    fn get_fetched_option_sets() -> Result<String, wit_types::ExtensionError> {
        get_ext()
            .get_fetched_option_sets()
            .map_err(|e| e.into_wit())
    }

    fn get_manga_details(manga_id: String) -> Result<MangaInfo, wit_types::ExtensionError> {
        get_ext()
            .get_manga_details(&manga_id)
            .map_err(|e| e.into_wit())
    }

    fn get_chapter_list(
        manga_id: String,
        page: i32,
        page_size: Option<i32>,
        sort: Option<String>,
    ) -> Result<ChapterList, wit_types::ExtensionError> {
        get_ext()
            .get_chapter_list(&manga_id, page, page_size, sort)
            .map_err(|e| e.into_wit())
    }
    async fn get_chapter_list_stream(
        manga_id: String,
        sort: Option<String>,
    ) -> kani_shared::StreamReader<Result<ChapterInfo, wit_types::ExtensionError>> {
        kani_shared::bridge_chapter_list_stream(get_ext(), manga_id, sort)
    }

    fn get_chapter_sort_list() -> Result<Vec<wit_types::SortOption>, wit_types::ExtensionError> {
        get_ext().get_chapter_sort_list().map_err(|e| e.into_wit())
    }

    fn get_pages(
        manga_id: String,
        chapter_id: String,
    ) -> Result<Chapter, wit_types::ExtensionError> {
        get_ext()
            .get_pages(&manga_id, &chapter_id)
            .map_err(|e| e.into_wit())
    }

    fn get_preferences() -> Result<Vec<PreferenceSpec>, wit_types::ExtensionError> {
        get_ext().get_preferences().map_err(|e| e.into_wit())
    }

    fn get_url(manga_id: String) -> Result<String, wit_types::ExtensionError> {
        get_ext().get_url(&manga_id).map_err(|e| e.into_wit())
    }
}

// ── MangaExtension impl ───────────────────────────────────────────────────────

impl MangaExtension for Comix {
    fn name(&self) -> &str {
        "Comix"
    }

    fn get_popular_manga(
        &self,
        page: i32,
        page_size: i32,
        filters: &[ActiveFilter],
    ) -> ExtensionResult<MangaList> {
        let req = self.build_manga_request(None, page, page_size, filters)?;
        self.fetch_manga_list(req)
    }

    fn search_manga(
        &self,
        query: &str,
        page: i32,
        page_size: i32,
        filters: &[ActiveFilter],
    ) -> ExtensionResult<MangaList> {
        let req = self.build_manga_request(Some(query), page, page_size, filters)?;
        self.fetch_manga_list(req)
    }

    fn get_manga_details(&self, manga_id: &str) -> ExtensionResult<MangaInfo> {
        let (hid, _) = id_parts(manga_id);
        let quality = Self::poster_quality();
        let poster_field = format!("/result/poster/{}", quality);

        let req = HttpRequest::get(format!("https://comix.to/api/v1/manga/{}", hid));
        let bp = BlueprintBuilder::new("")
            .request(req)
            .field("title", Expr::json_root("/result/title").str_val())
            .field_opt("desc", Expr::json_root("/result/synopsisHtml").str_val())
            .field_opt("cover", Expr::json_root(&poster_field).str_val())
            .field(
                "status",
                Expr::json_root("/result/status")
                    .str_val()
                    .lookup(vec![
                        ("RELEASING", "ongoing"),
                        ("FINISHED", "completed"),
                        ("HIATUS", "hiatus"),
                        ("DISCONTINUED", "cancelled"),
                    ])
                    .fallback_str("unknown"),
            )
            .field(
                "authors",
                Expr::json_root("/result/authors").map(Expr::var("$item").ptr("/title").str_val()),
            )
            .field(
                "artists",
                Expr::json_root("/result/artists").map(Expr::var("$item").ptr("/title").str_val()),
            )
            .field(
                "genres",
                Expr::json_root("/result/genres").map(Expr::var("$item").ptr("/title").str_val()),
            )
            .field(
                "demos",
                Expr::json_root("/result/demographics")
                    .map(Expr::var("$item").ptr("/title").str_val()),
            )
            .field(
                "tag_list",
                Expr::json_root("/result/tags").map(Expr::var("$item").ptr("/name").str_val()),
            )
            .field_opt("type_tag", Expr::json_root("/result/type").str_val())
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

        let mut tags = row.get_array_of_strings("/genres");
        tags.extend(row.get_array_of_strings("/demos"));
        tags.extend(row.get_array_of_strings("/tag_list"));
        if let Some(t) = row.get_str("/type_tag") {
            tags.insert(0, t);
        }

        Ok(MangaInfo {
            id: manga_id.to_string(),
            title: row.require_str("/title")?,
            description: row.get_str("/desc"),
            status,
            cover_url: row.get_str("/cover"),
            authors: row.get_array_of_strings("/authors"),
            artists: row.get_array_of_strings("/artists"),
            tags,
        })
    }

    fn get_chapter_list(
        &self,
        manga_id: &str,
        _page: i32,
        _page_size: Option<i32>,
        _sort: Option<String>,
    ) -> ExtensionResult<ChapterList> {
        let (_, manga_slug) = id_parts(manga_id);
        let page_url = format!("https://comix.to/title/{}", manga_slug);

        let init_script = r#"
(function () {
    var allItems = [];
    var lastMeta = null;
    var sent = false;
    var seenParses = 0;

    function log(msg) {
        try { console.log('[comix-shim] ' + msg); } catch (e) {}
    }

    function submit() {
        if (sent) return;
        sent = true;
        var meta = lastMeta || { hasNext: false };
        log('submitting ' + allItems.length + ' items');
        passPayload(JSON.stringify({ result: { items: allItems, meta: meta } }));
    }

    function isChapterPayload(parsed) {
        if (!parsed || !parsed.result) return false;
        var items = parsed.result.items;
        if (!Array.isArray(items) || items.length === 0) return false;
        var first = items[0];
        if (!first) return false;
        return first.mangaId !== undefined || first.manga_id !== undefined;
    }

    function handleChapters(parsed) {
        if (sent) return;
        try {
            if (!isChapterPayload(parsed)) return;
            var items = parsed.result.items;
            var meta = parsed.result.meta || parsed.result.pagination || null;
            lastMeta = meta;
            allItems = allItems.concat(items);
            log('captured ' + items.length + ' chapters (total=' + allItems.length +
                ', meta=' + (meta ? JSON.stringify(meta).slice(0, 80) : 'none') + ')');

            var hasNext = meta && (meta.hasNext === true || meta.has_next === true);
            if (!hasNext) {
                submit();
                return;
            }

            resetPayloadTimer();
            var tries = 0;
            var iv = setInterval(function () {
                if (sent) { clearInterval(iv); return; }
                var btn = document.querySelector('.mchap-foot button[aria-label*=Next]')
                       || document.querySelector('button[aria-label*=Next]');
                if (btn && !btn.disabled) {
                    log('clicking Next button (try=' + tries + ')');
                    btn.click();
                    clearInterval(iv);
                } else if (++tries > 60) {
                    clearInterval(iv);
                    log('no Next button found after ' + tries + ' tries — submitting partial');
                    submit();
                }
            }, 200);
        } catch (e) {
            log('handleChapters error: ' + e);
        }
    }

    var origParse = JSON.parse;
    JSON.parse = function (text) {
        var result = origParse.apply(this, arguments);
        if (!sent) {
            seenParses++;
            if (seenParses <= 30) {
                try {
                    var topKeys = result && typeof result === 'object'
                        ? Object.keys(result).slice(0, 5).join(',')
                        : typeof result;
                    var resKeys = result && result.result && typeof result.result === 'object'
                        ? Object.keys(result.result).slice(0, 8).join(',')
                        : 'none';
                    log('parse #' + seenParses + ' top=[' + topKeys + '] result=[' + resKeys + ']');
                } catch (e) {}
            }
            handleChapters(result);
        }
        return result;
    };

    // Bump the site's default per-page chapter limit (20) to 100 so manga with
    // hundreds of chapters need ~5× fewer Next-button clicks. Matches Mihon's
    // approach; the API accepts arbitrarily large limits.
    var origOpen = XMLHttpRequest.prototype.open;
    XMLHttpRequest.prototype.open = function (method, url) {
        if (typeof url === 'string'
            && url.indexOf('/chapters') !== -1
            && /[?&]limit=\d+/.test(url)) {
            arguments[1] = url.replace(/([?&]limit=)\d+/, '$1100');
        }
        return origOpen.apply(this, arguments);
    };

    log('hook installed');
})();
"#;

        let payload =
            v8_context::capture_page_payload(&page_url, init_script, 60_000).map_err(|e| {
                ExtensionError::unknown(format!("comix: chapter list capture failed: {e}"))
            })?;

        let handle = JsonHandle::parse(payload.as_bytes())?;
        let bp = BlueprintBuilder::new("/result/items")
            .field(
                "id",
                Expr::self_ref()
                    .ptr("/id")
                    .int_val()
                    .fallback(Expr::num(0.0)),
            )
            .field(
                "number",
                Expr::self_ref()
                    .ptr("/number")
                    .float_val()
                    .fallback(Expr::num(0.0)),
            )
            .field_opt("title", Expr::self_ref().ptr("/name").str_val())
            .field_opt("slug", Expr::self_ref().ptr("/slug").str_val())
            .field_opt("volume", Expr::self_ref().ptr("/volume").int_val())
            .field_opt("scanlator", Expr::self_ref().ptr("/group/name").str_val())
            .field(
                "language",
                Expr::self_ref().ptr("/language").str_val().fallback_str(""),
            )
            .scalar_opt(
                "has_next",
                Expr::json_root("/result/meta/hasNext").bool_val(),
            )
            .build();

        let rows = extract::json(Some(handle.raw_handle()), &bp)?;
        let has_next = rows.get_scalar_bool("has_next");
        let count = rows.rows_len();

        let chapters: Vec<ChapterInfo> = (0..count)
            .filter_map(|i| {
                let row = rows.rows_get(i).ok()?;
                let numeric_id = row.get_i64("/id").unwrap_or(0);
                let number = row.get_f64("/number").unwrap_or(0.0);
                let ch_slug = row.get_str("/slug").unwrap_or_else(|| {
                    let n = if number.fract() == 0.0 {
                        format!("{}", number as i64)
                    } else {
                        format!("{:.1}", number).replace('.', "-")
                    };
                    format!("{}-chapter-{}", numeric_id, n)
                });
                Some(ChapterInfo {
                    id: encode_manga_id(&format!("{}|{}", numeric_id, ch_slug)),
                    number,
                    title: row.get_str("/title"),
                    volume: row.get_i64("/volume").map(|n| n as i32),
                    scanlator: row.get_str("/scanlator"),
                    date_uploaded: None,
                    language: row.get_str("/language").unwrap_or_default(),
                    page_count: None,
                })
            })
            .collect();

        Ok(ChapterList {
            chapters,
            has_next_page: has_next,
            total_pages: None,
        })
    }

    fn get_pages(&self, manga_id: &str, chapter_id: &str) -> ExtensionResult<Chapter> {
        let (_, manga_slug) = id_parts(manga_id);
        let (_, ch_slug) = id_parts(chapter_id);
        let page_url = format!("https://comix.to/title/{}/{}", manga_slug, ch_slug);

        let init_script = r#"
(function () {
    var sent = false;
    var seenParses = 0;

    function log(msg) {
        try { console.log('[comix-shim] ' + msg); } catch (e) {}
    }

    var origParse = JSON.parse;
    JSON.parse = function (text) {
        var result = origParse.apply(this, arguments);
        if (!sent) {
            seenParses++;
            if (seenParses <= 30) {
                try {
                    var resKeys = result && result.result && typeof result.result === 'object'
                        ? Object.keys(result.result).slice(0, 8).join(',')
                        : 'none';
                    log('parse #' + seenParses + ' result=[' + resKeys + ']');
                } catch (e) {}
            }
            try {
                if (result && result.result && result.result.pages) {
                    sent = true;
                    log('captured pages payload (' + String(text).length + ' chars)');
                    passPayload(text);
                }
            } catch (e) {}
        }
        return result;
    };
    log('pages hook installed');
})();
"#;

        let payload = v8_context::capture_page_payload(&page_url, init_script, 30_000)
            .map_err(|e| ExtensionError::unknown(format!("comix: pages capture failed: {e}")))?;

        let handle = JsonHandle::parse(payload.as_bytes())
            .map_err(|e| ExtensionError::unknown(format!("comix: pages parse error: {e}")))?;

        let base = handle.get_str("/result/pages/baseUrl").unwrap_or_default();
        let base = base.trim_end_matches('/');

        let bp = BlueprintBuilder::new("/result/pages/items")
            .field("url", Expr::self_ref().ptr("/url").str_val())
            .field("index", Expr::index())
            .build();

        let rows = extract::json(Some(handle.raw_handle()), &bp)?;
        let count = rows.rows_len();
        if count == 0 {
            return Err(ExtensionError::unknown("No pages found".into()));
        }

        Ok(Chapter {
            pages: (0..count)
                .filter_map(|i| {
                    let row = rows.rows_get(i).ok()?;
                    let url = row.require_str("/url").ok()?;
                    let full = if url.starts_with("http") {
                        url
                    } else {
                        format!("{}/{}", base, url.trim_start_matches('/'))
                    };
                    Some(Page {
                        index: row.get_i64("/index").unwrap_or(i as i64) as i32,
                        url: full,
                        transform: Some("lcg-tile-5x5-from-header".to_string()),
                    })
                })
                .collect(),
        })
    }

    fn get_filter_list(&self) -> ExtensionResult<FilterList> {
        use kani_shared::filter_list;
        let mut list = filter_list! {
            "sort", "Sort", Select, [
                ("Score (Desc)",            "order[score]_desc"),
                ("Score (Asc)",             "order[score]_asc"),
                ("Latest Update (Desc)",    "order[chapter_updated_at]_desc"),
                ("Latest Update (Asc)",     "order[chapter_updated_at]_asc"),
                ("Title (A-Z)",             "order[title]_asc"),
                ("Title (Z-A)",             "order[title]_desc"),
                ("Year (Newest First)",     "order[year]_desc"),
                ("Year (Oldest First)",     "order[year]_asc"),
                ("Follows (Most)",          "order[follows]_desc"),
                ("Chapter Count (Most)",    "order[chapters]_desc"),
                ("Rating (Best)",           "order[rating]_desc"),
                ("Views (Most)",            "order[views]_desc"),
                ("Oldest Added",            "order[created_at]_asc"),
            ];

            "content_rating", "Content Rating", Select, [
                ("Show all",           ""),
                ("Safe only",          "safe"),
                ("Up to Suggestive",   "suggestive"),
                ("Up to Erotica",      "erotica"),
                ("Up to Pornographic", "pornographic"),
            ];

            "status", "Status", Select, [
                ("All",       ""),
                ("Ongoing",   "ongoing"),
                ("Completed", "completed"),
                ("Hiatus",    "hiatus"),
                ("Abandoned", "abandoned"),
            ];

            "type", "Type", Multiselect, [
                ("Manga",  "manga"),
                ("Manhwa", "manhwa"),
                ("Manhua", "manhua"),
                ("Other",  "other"),
            ];

            "demographics", "Demographics", Multiselect, [
                ("Shounen", "2"),
                ("Shoujo",  "1"),
                ("Seinen",  "4"),
                ("Josei",   "3"),
            ];

            "genres_include",  "Genres Include",  Multiselect, splice: GENRES;
            "genres_exclude",  "Genres Exclude",  Multiselect, splice: GENRES;
            "formats_include", "Formats Include", Multiselect, splice: FORMATS;
            "formats_exclude", "Formats Exclude", Multiselect, splice: FORMATS;

            "genres_mode", "Match Mode", Select, [
                ("Match any (OR)",  "OR"),
                ("Match all (AND)", "AND"),
            ];

            "min_chapters", "Minimum Chapters", TextInput;
            "tags", "Tags", TextInput;
            "author", "Author", TextInput;
            "artist", "Artist", TextInput;
        };

        list.filters
            .push(make_year_select("year_from", "Year From"));
        list.filters.push(make_year_select("year_to", "Year To"));

        Ok(list)
    }

    fn get_preferences(&self) -> ExtensionResult<Vec<PreferenceSpec>> {
        use kani_shared::preference_list;
        let mut specs = preference_list! {
            PREF_POSTER_QUALITY, "Thumbnail quality", Select,
                [("Small", "small"), ("Medium", "medium"), ("Large", "large")],
                default: "large",
                description: "Quality of thumbnail images in browse lists.";

            PREF_CONTENT_RATING, "Default content rating", Select,
                [
                    ("Show all",           ""),
                    ("Safe only",          "safe"),
                    ("Up to Suggestive",   "suggestive"),
                    ("Up to Erotica",      "erotica"),
                    ("Up to Pornographic", "pornographic"),
                ],
                default: "suggestive",
                description: "Maximum content rating shown in popular and search results. Overridden by the Content Rating search filter.";
        };
        specs.push(PreferenceSpec {
            key:         PREF_BLOCKED_GENRES.to_string(),
            label:       "Blocked genres".to_string(),
            kind:        PrefKind::MultiValueList,
            options:     GENRES.iter().map(|(n, v)| (n.to_string(), v.to_string())).collect(),
            default:     "[]".to_string(),
            description: Some("Genres permanently excluded from all results. Can be overridden per-search via the Genres Include filter.".to_string()),
            secret:      false,
        });
        Ok(specs)
    }

    fn get_chapter_sort_list(&self) -> ExtensionResult<Vec<wit_types::SortOption>> {
        Ok(chapter_sort_list! {
            "number",     "Chapter";
            "volume",     "Volume";
            "created_at", "Last Updated";
        })
    }

    fn get_url(&self, manga_id: &str) -> ExtensionResult<String> {
        let (_, page_slug) = id_parts(manga_id);
        Ok(format!("https://comix.to/title/{}", page_slug))
    }
}

// ── Year filter helper (dynamic — cannot use filter_list! macro) ──────────────

fn make_year_select(id: &str, name: &str) -> FilterDef {
    let mut options = Vec::with_capacity((YEAR_MAX - YEAR_MIN + 2) as usize);
    options.push(FilterOption {
        filter_name: id.to_string(),
        name: "Any".to_string(),
        value: "".to_string(),
    });
    for y in YEAR_MIN..=YEAR_MAX {
        let ys = y.to_string();
        options.push(FilterOption {
            filter_name: id.to_string(),
            name: ys.clone(),
            value: ys,
        });
    }
    FilterDef {
        id: id.to_string(),
        name: name.to_string(),
        tag: FilterTypeTag::Select,
        options,
        default_value: None,
        semantic: None,
    }
}

// ── WASM singleton ────────────────────────────────────────────────────────────

use std::sync::OnceLock;
static EXTENSION: OnceLock<Comix> = OnceLock::new();
fn get_ext() -> &'static Comix {
    EXTENSION.get_or_init(Comix::default)
}
bindings::export!(Comix);

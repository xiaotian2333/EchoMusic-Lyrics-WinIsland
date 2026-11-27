use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;

// 仅内嵌字体启用时使用；仓库不含 font.otf（见 build.rs），CI 上该 cfg 未设置
#[cfg(has_builtin_font)]
use once_cell::sync::Lazy;
use skia_safe::{
    Canvas, Font, FontMgr, FontStyle, FourByteTag, Paint, Typeface,
    font_arguments::{FontArguments, VariationPosition, variation_position},
};

use crate::core::persistence::load_config;

static GLOBAL_FONT_MANAGER: OnceLock<FontManager> = OnceLock::new();

type TextGroup = (String, Typeface, bool);
type TextGroups = Vec<TextGroup>;
type TextCacheValue = (f32, TextGroups);
type TextCacheMap = HashMap<u64, TextCacheValue>;

pub struct DrawTextCachedParams<'a> {
    pub canvas: &'a Canvas,
    pub text: &'a str,
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub bold: bool,
    pub paint: &'a Paint,
}

pub struct FontManager {
    _marker: (),
}

thread_local! {
    static FONT_MGR: FontMgr = FontMgr::new();
    static FALLBACK_CACHE: RefCell<HashMap<(char, u32), Typeface>> = RefCell::new(HashMap::new());
    static TEXT_CACHE: RefCell<TextCacheMap> = RefCell::new(HashMap::new());
    static CUSTOM_TYPEFACE: RefCell<Option<(String, Typeface)>> = const { RefCell::new(None) };
    static WEIGHT_CLONE_CACHE: RefCell<HashMap<(u32, i32), Typeface>> = RefCell::new(HashMap::new());
    /// 自定义字体路径快照：渲染热路径禁止读盘解析配置，
    /// 快照由 refresh_custom_font 作为唯一失效钩子维护（None 表示尚未初始化）
    static CUSTOM_FONT_PATH_SNAPSHOT: RefCell<Option<Option<String>>> = const { RefCell::new(None) };
}

#[cfg(has_builtin_font)]
static BUILTIN_FONT_BYTES: Lazy<&[u8]> =
    Lazy::new(|| &include_bytes!("../../resources/font.otf")[..]);
#[cfg(has_builtin_font)]
static BUILTIN_TYPEFACE: OnceLock<Typeface> = OnceLock::new();

const FALLBACK_CACHE_LIMIT: usize = 2000;
const TEXT_CACHE_LIMIT: usize = 500;

fn evict_one_if_full<K, V>(cache: &mut HashMap<K, V>, limit: usize)
where
    K: Clone + std::cmp::Eq + std::hash::Hash,
{
    if cache.len() > limit
        && let Some(key) = cache.keys().next().cloned()
    {
        cache.remove(&key);
    }
}

fn hash_cache_key(text: &str, bold: u32, size_key: i32) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    bold.hash(&mut hasher);
    size_key.hash(&mut hasher);
    hasher.finish()
}

fn style_to_key(style: FontStyle) -> u32 {
    let weight = *style.weight() as u32;
    let width = *style.width() as u32;
    let slant = style.slant() as u32;
    (weight << 16) | (width << 8) | slant
}

fn needs_synthetic_bold(tf: &Typeface, style: FontStyle) -> bool {
    *style.weight() >= 600 && *tf.font_style().weight() < 600
}

fn load_typeface_from_data(data: &[u8]) -> Option<Typeface> {
    FONT_MGR.with(|mgr| mgr.new_from_data(data, None))
}

fn load_typeface_from_path(path: &str) -> Option<Typeface> {
    let data = std::fs::read(path).ok()?;
    load_typeface_from_data(&data)
}

pub fn can_load_font_file(path: &str) -> bool {
    load_typeface_from_path(path).is_some()
}

fn current_custom_font_path() -> Option<String> {
    CUSTOM_FONT_PATH_SNAPSHOT.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(load_config().custom_font_path.clone());
        }
        slot.as_ref().and_then(|path| path.clone())
    })
}

pub fn get_custom_font_data() -> Option<Vec<u8>> {
    let path = current_custom_font_path()?;
    let data = std::fs::read(path).ok()?;
    load_typeface_from_data(&data)?;
    Some(data)
}

fn get_custom_typeface() -> Option<Typeface> {
    let path = current_custom_font_path()?;
    CUSTOM_TYPEFACE.with(|cache| {
        let mut cache_mut = cache.borrow_mut();
        if let Some((ref cached_path, ref tf)) = *cache_mut
            && cached_path == &path
        {
            return Some(tf.clone());
        }
        if let Some(tf) = load_typeface_from_path(&path) {
            *cache_mut = Some((path, tf.clone()));
            return Some(tf);
        }
        None
    })
}

/// 获取编译时内嵌的字体原始字节数据。
/// 仅在 cfg(has_builtin_font) 时存在；否则返回 None。
pub fn get_builtin_font_data() -> Option<&'static [u8]> {
    #[cfg(has_builtin_font)]
    {
        Some(*BUILTIN_FONT_BYTES)
    }
    #[cfg(not(has_builtin_font))]
    {
        None
    }
}

/// 尝试获取编译时内嵌的内置字体。
/// 仅在 cfg(has_builtin_font) 时存在；否则返回 None。
fn get_builtin_typeface() -> Option<&'static Typeface> {
    #[cfg(has_builtin_font)]
    {
        Some(BUILTIN_TYPEFACE.get_or_init(|| {
            FONT_MGR
                .with(|mgr| mgr.new_from_data(*BUILTIN_FONT_BYTES, None))
                .expect("MiSans 内嵌字体加载失败")
        }))
    }
    #[cfg(not(has_builtin_font))]
    {
        None
    }
}

/// 如果字型支持 wght 轴且请求字重在范围内，返回调整后的克隆；
/// 否则直接返回原字型的 clone。
fn with_weight(tf: &Typeface, weight: i32) -> Typeface {
    let key = (tf.unique_id(), weight);
    if let Some(cached) = WEIGHT_CLONE_CACHE.with(|c| c.borrow().get(&key).cloned()) {
        return cached;
    }

    let has_wght = tf.variation_design_parameters().is_some_and(|params| {
        params.iter().any(|axis| {
            axis.tag == FourByteTag::from_chars('w', 'g', 'h', 't')
                && weight as f32 >= axis.min
                && weight as f32 <= axis.max
        })
    });

    let result = if has_wght {
        let coord = variation_position::Coordinate {
            axis: FourByteTag::from_chars('w', 'g', 'h', 't'),
            value: weight as f32,
        };
        let pos = VariationPosition {
            coordinates: std::slice::from_ref(&coord),
        };
        let args = FontArguments::new().set_variation_design_position(pos);
        tf.clone_with_arguments(&args).unwrap_or_else(|| tf.clone())
    } else {
        tf.clone()
    };

    WEIGHT_CLONE_CACHE.with(|c| c.borrow_mut().insert(key, result.clone()));
    result
}

fn typeface_has_char(tf: &Typeface, c: char) -> bool {
    let mut glyphs = [0u16; 1];
    tf.unichars_to_glyphs(&[c as i32], &mut glyphs);
    glyphs[0] != 0
}

fn typeface_has_text(tf: &Typeface, text: &str) -> bool {
    text.chars().all(|c| typeface_has_char(tf, c))
}

fn styled_typeface(tf: &Typeface, style: FontStyle) -> (Typeface, bool) {
    let tf = with_weight(tf, *style.weight());
    let embolden = needs_synthetic_bold(&tf, style);
    (tf, embolden)
}

fn get_typeface_for_char(c: char, style: FontStyle) -> (Typeface, bool) {
    let s_key = style_to_key(style);
    FALLBACK_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        evict_one_if_full(&mut cache, FALLBACK_CACHE_LIMIT);
        if let Some(tf) = cache.get(&(c, s_key)) {
            let embolden = needs_synthetic_bold(tf, style);
            return (tf.clone(), embolden);
        }

        // 优先尝试用户设置的自定义字体
        if let Some(base) = get_custom_typeface()
            && typeface_has_char(&base, c)
        {
            let (tf, embolden) = styled_typeface(&base, style);
            cache.insert((c, s_key), tf.clone());
            return (tf, embolden);
        }

        // 其次尝试内嵌字体
        if let Some(base) = get_builtin_typeface()
            && typeface_has_char(base, c)
        {
            let (tf, embolden) = styled_typeface(base, style);
            cache.insert((c, s_key), tf.clone());
            return (tf, embolden);
        }

        // 最后系统 fallback
        let tf = FONT_MGR
            .with(|mgr| {
                mgr.match_family_style_character("", style, &["zh-CN", "ja-JP", "en-US"], c as i32)
            })
            .unwrap_or_else(|| FONT_MGR.with(|mgr| mgr.legacy_make_typeface(None, style).unwrap()));
        let embolden = needs_synthetic_bold(&tf, style);
        cache.insert((c, s_key), tf.clone());
        (tf, embolden)
    })
}

fn is_ascii_text(text: &str) -> bool {
    text.bytes().all(|b| b.is_ascii())
}

/// 计算文本分组和总宽度。
/// ASCII 文本优先使用单字型快速路径，减少逐字符查找。
fn compute_text_groups(text: &str, size: f32, style: FontStyle) -> (f32, TextGroups) {
    let mut current_w = 0.0;
    let mut groups: TextGroups = Vec::new();

    if is_ascii_text(text) {
        let custom = get_custom_typeface();
        let single_typeface = if let Some(base) = custom.as_ref() {
            if typeface_has_text(base, text) {
                Some(styled_typeface(base, style))
            } else {
                None
            }
        } else if let Some(base) = get_builtin_typeface() {
            Some(styled_typeface(base, style))
        } else {
            let tf = FONT_MGR.with(|mgr| {
                mgr.match_family_style("Microsoft YaHei", style)
                    .or_else(|| mgr.match_family_style("Segoe UI", style))
                    .unwrap_or_else(|| mgr.legacy_make_typeface(None, style).unwrap())
            });
            let embolden = needs_synthetic_bold(&tf, style);
            Some((tf, embolden))
        };

        if let Some((tf, embolden)) = single_typeface {
            let mut font = Font::from_typeface(tf.clone(), size);
            if embolden {
                font.set_embolden(true);
            }
            let (w, _) = font.measure_str(text, None);
            current_w += w;
            groups.push((text.to_string(), tf, embolden));
            return (current_w, groups);
        }
    }

    let mut current_group = String::new();
    let mut last_tf: Option<Typeface> = None;
    let mut last_embolden = false;
    for c in text.chars() {
        let (tf, embolden) = get_typeface_for_char(c, style);
        if let Some(ref ltf) = last_tf
            && (ltf.unique_id() != tf.unique_id() || last_embolden != embolden)
        {
            groups.push((current_group.clone(), ltf.clone(), last_embolden));
            current_group.clear();
        }
        last_tf = Some(tf);
        last_embolden = embolden;
        current_group.push(c);
    }
    if let Some(ltf) = last_tf {
        groups.push((current_group, ltf, last_embolden));
    }

    for (s, tf, embolden) in &groups {
        let mut font = Font::from_typeface(tf.clone(), size);
        if *embolden {
            font.set_embolden(true);
        }
        let (w, _) = font.measure_str(s, None);
        current_w += w;
    }

    (current_w, groups)
}

impl FontManager {
    pub fn global() -> &'static FontManager {
        GLOBAL_FONT_MANAGER.get_or_init(|| FontManager { _marker: () })
    }

    pub fn measure_text_cached(&self, text: &str, size: f32, style: FontStyle) -> f32 {
        let cache_key = hash_cache_key(text, style_to_key(style), (size * 100.0).round() as i32);
        TEXT_CACHE.with(|cache| {
            let mut cache_mut = cache.borrow_mut();
            evict_one_if_full(&mut cache_mut, TEXT_CACHE_LIMIT);
            let entry = cache_mut.entry(cache_key).or_insert_with(|| {
                let (width, groups) = compute_text_groups(text, size, style);
                (width, groups)
            });
            entry.0
        })
    }

    pub fn draw_text_cached(&self, params: DrawTextCachedParams<'_>) {
        let style = if params.bold {
            FontStyle::bold()
        } else {
            FontStyle::normal()
        };
        let cache_key = hash_cache_key(params.text, params.bold as u32, params.size as i32);
        TEXT_CACHE.with(|cache| {
            let mut cache_mut = cache.borrow_mut();
            evict_one_if_full(&mut cache_mut, TEXT_CACHE_LIMIT);
            let entry = cache_mut.entry(cache_key).or_insert_with(|| {
                let (_, groups) = compute_text_groups(params.text, params.size, style);
                (0.0, groups)
            });
            let (_, groups) = entry;
            let mut x = params.x;
            let y = params.y.round();
            for (s, tf, embolden) in groups {
                let mut font = Font::from_typeface(tf.clone(), params.size);
                if *embolden {
                    font.set_embolden(true);
                }
                params
                    .canvas
                    .draw_str(&**s, (x.round(), y), &font, params.paint);
                let (w, _) = font.measure_str(&**s, None);
                x += w;
            }
        });
    }

    pub fn refresh_custom_font(&self) {
        // 路径快照以本函数为唯一失效钩子：先重新读一次配置刷新快照，
        // 再清空所有字体缓存
        CUSTOM_FONT_PATH_SNAPSHOT.with(|slot| {
            *slot.borrow_mut() = Some(load_config().custom_font_path.clone());
        });
        CUSTOM_TYPEFACE.with(|cache| {
            *cache.borrow_mut() = None;
        });
        TEXT_CACHE.with(|cache| {
            cache.borrow_mut().clear();
        });
        FALLBACK_CACHE.with(|cache| {
            cache.borrow_mut().clear();
        });
        WEIGHT_CLONE_CACHE.with(|cache| {
            cache.borrow_mut().clear();
        });
    }
}

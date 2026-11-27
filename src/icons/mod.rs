pub mod arrows;
pub mod controls;
pub mod music;
pub mod play_mode;
pub mod volume;

use std::cell::RefCell;
use std::collections::HashMap;

use skia_safe::Path;

thread_local! {
    /// SVG 路径解析结果缓存：图标在每帧绘制路径上被反复使用，避免重复解析字符串
    static SVG_PATH_CACHE: RefCell<HashMap<&'static str, Option<Path>>> = RefCell::new(HashMap::new());
}

/// 取（并缓存）SVG 路径；解析失败也缓存，避免持续重试。
pub(crate) fn cached_svg_path(path_data: &'static str) -> Option<Path> {
    SVG_PATH_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(path) = cache.get(path_data) {
            return path.clone();
        }
        let parsed = Path::from_svg(path_data);
        cache.insert(path_data, parsed.clone());
        parsed
    })
}

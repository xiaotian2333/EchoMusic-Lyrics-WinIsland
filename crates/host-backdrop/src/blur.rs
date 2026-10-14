//! 宿主背板的高斯模糊效果（移植自 WinIsland 的 backdrop/blur.rs）。
//!
//! Windows.UI.Composition 只提供效果工厂，没有现成的高斯模糊效果对象，
//! 因此这里用 `#[implement]` 手写一个实现 `IGraphicsEffectD2D1Interop` 的
//! D2D1GaussianBlur 包装，再通过 `CompositionEffectBrush` 作用在宿主背板画笔上，
//! 得到可实时调参的毛玻璃磨砂程度。

use std::sync::Mutex;

use windows::Foundation::{IPropertyValue, PropertyValue};
use windows::Graphics::Effects::{
    IGraphicsEffect, IGraphicsEffect_Impl, IGraphicsEffectSource, IGraphicsEffectSource_Impl,
};
use windows::UI::Composition::{
    CompositionBrush, CompositionEffectBrush, CompositionEffectSourceParameter, Compositor,
};
use windows::Win32::Foundation::E_INVALIDARG;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_BORDER_MODE, D2D1_BORDER_MODE_HARD};
use windows::Win32::Graphics::Direct2D::{
    CLSID_D2D1GaussianBlur, D2D1_GAUSSIANBLUR_OPTIMIZATION_BALANCED,
};
use windows::Win32::System::WinRT::Graphics::Direct2D::{
    GRAPHICS_EFFECT_PROPERTY_MAPPING, GRAPHICS_EFFECT_PROPERTY_MAPPING_DIRECT,
    IGraphicsEffectD2D1Interop, IGraphicsEffectD2D1Interop_Impl,
};
use windows::core::{Error, GUID, HSTRING, Interface, PCWSTR, Result};
use windows_collections::IIterable;

const EFFECT_NAME: &str = "Blur";
const SOURCE_NAME: &str = "backdrop";
const BLUR_AMOUNT: &str = "Blur.BlurAmount";
const PROPERTIES: [&str; 3] = ["BlurAmount", "Optimization", "BorderMode"];

/// 作用在宿主背板画笔上的高斯模糊效果画笔。
pub(super) struct BackdropBlur {
    brush: CompositionEffectBrush,
}

impl BackdropBlur {
    pub(super) fn new(compositor: &Compositor, backdrop: &CompositionBrush) -> Result<Self> {
        let effect: IGraphicsEffect = GaussianBlur {
            name: Mutex::new(HSTRING::from(EFFECT_NAME)),
            source: CompositionEffectSourceParameter::Create(&HSTRING::from(SOURCE_NAME))?
                .cast()?,
            border: D2D1_BORDER_MODE_HARD,
        }
        .into();
        // 只有登记为可动画属性的模糊量才能在运行时通过 InsertScalar 修改
        let animatable = IIterable::<HSTRING>::from(vec![HSTRING::from(BLUR_AMOUNT)]);
        let brush = compositor
            .CreateEffectFactoryWithProperties(&effect, &animatable)?
            .CreateBrush()?;
        brush.SetSourceParameter(&HSTRING::from(SOURCE_NAME), backdrop)?;
        Ok(Self { brush })
    }

    pub(super) fn brush(&self) -> Result<CompositionBrush> {
        self.brush.cast()
    }

    pub(super) fn set_amount(&self, amount: f32) -> Result<()> {
        self.brush
            .Properties()?
            .InsertScalar(&HSTRING::from(BLUR_AMOUNT), amount.max(0.0))
    }
}

#[windows::core::implement(IGraphicsEffect, IGraphicsEffectSource, IGraphicsEffectD2D1Interop)]
struct GaussianBlur {
    name: Mutex<HSTRING>,
    source: IGraphicsEffectSource,
    border: D2D1_BORDER_MODE,
}

impl IGraphicsEffectSource_Impl for GaussianBlur_Impl {}

impl IGraphicsEffect_Impl for GaussianBlur_Impl {
    fn Name(&self) -> Result<HSTRING> {
        Ok(self
            .name
            .lock()
            .map(|name| name.clone())
            .unwrap_or_default())
    }

    fn SetName(&self, name: &HSTRING) -> Result<()> {
        if let Ok(mut current) = self.name.lock() {
            *current = name.clone();
        }
        Ok(())
    }
}

impl IGraphicsEffectD2D1Interop_Impl for GaussianBlur_Impl {
    fn GetEffectId(&self) -> Result<GUID> {
        Ok(CLSID_D2D1GaussianBlur)
    }

    fn GetNamedPropertyMapping(
        &self,
        name: &PCWSTR,
        index: *mut u32,
        mapping: *mut GRAPHICS_EFFECT_PROPERTY_MAPPING,
    ) -> Result<()> {
        // SAFETY: 合成器保证传入以空字符结尾的属性名指针。
        let name = unsafe { name.to_string() }.map_err(|_| Error::from(E_INVALIDARG))?;
        let position = PROPERTIES
            .iter()
            .position(|property| property.eq_ignore_ascii_case(&name))
            .ok_or_else(|| Error::from(E_INVALIDARG))?;
        // SAFETY: 两个输出指针均由合成器在本次调用中提供。
        unsafe {
            *index = position as u32;
            *mapping = GRAPHICS_EFFECT_PROPERTY_MAPPING_DIRECT;
        }
        Ok(())
    }

    fn GetPropertyCount(&self) -> Result<u32> {
        Ok(PROPERTIES.len() as u32)
    }

    fn GetProperty(&self, index: u32) -> Result<IPropertyValue> {
        match index {
            0 => PropertyValue::CreateSingle(0.0)?.cast(),
            1 => PropertyValue::CreateUInt32(D2D1_GAUSSIANBLUR_OPTIMIZATION_BALANCED.0 as u32)?
                .cast(),
            2 => PropertyValue::CreateUInt32(self.border.0 as u32)?.cast(),
            _ => Err(E_INVALIDARG.into()),
        }
    }

    fn GetSource(&self, index: u32) -> Result<IGraphicsEffectSource> {
        if index == 0 {
            Ok(self.source.clone())
        } else {
            Err(E_INVALIDARG.into())
        }
    }

    fn GetSourceCount(&self) -> Result<u32> {
        Ok(1)
    }
}

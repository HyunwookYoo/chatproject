#include "flutter_window.h"

#include <optional>
#include <algorithm>
#include <dwmapi.h>
#include <flutter/standard_method_codec.h>

#include "flutter/generated_plugin_registrant.h"

FlutterWindow::FlutterWindow(const flutter::DartProject& project)
    : project_(project) {}

FlutterWindow::~FlutterWindow() {}

bool FlutterWindow::OnCreate() {
  if (!Win32Window::OnCreate()) {
    return false;
  }

  // These DWM attributes are Set-only. This runner owns their known baseline.
  const BOOL disabled = FALSE;
  force_result_ = DwmSetWindowAttribute(GetHandle(), DWMWA_FORCE_ICONIC_REPRESENTATION, &disabled, sizeof(disabled));
  bitmap_result_ = DwmSetWindowAttribute(GetHandle(), DWMWA_HAS_ICONIC_BITMAP, &disabled, sizeof(disabled));
  baseline_ready_ = SUCCEEDED(force_result_) && SUCCEEDED(bitmap_result_);
  RECT frame = GetClientArea();

  // The size here must match the window dimensions to avoid unnecessary surface
  // creation / destruction in the startup path.
  flutter_controller_ = std::make_unique<flutter::FlutterViewController>(
      frame.right - frame.left, frame.bottom - frame.top, project_);
  // Ensure that basic setup of the controller was successful.
  if (!flutter_controller_->engine() || !flutter_controller_->view()) {
    return false;
  }
  RegisterPlugins(flutter_controller_->engine());
  account_channel_ = std::make_unique<flutter::MethodChannel<flutter::EncodableValue>>(
      flutter_controller_->engine()->messenger(), "chat_app/account_storage",
      &flutter::StandardMethodCodec::GetInstance());
  privacy_events_ = std::make_unique<flutter::MethodChannel<flutter::EncodableValue>>(
      flutter_controller_->engine()->messenger(), "chat_app/privacy_events",
      &flutter::StandardMethodCodec::GetInstance());
  account_channel_->SetMethodCallHandler([this](const auto& call, auto result) {
    if (call.method_name() == "setSensitiveScreen") {
      const auto* value = std::get_if<bool>(call.arguments());
      if (!value) { result->Error("invalid_argument"); return; }
      if (!SetSensitive(*value)) { result->Error("privacy_unavailable"); return; }
      result->Success();
      return;
    }
#ifndef NDEBUG
    if (call.method_name() == "privacyTest") {
      const auto* mode = std::get_if<std::string>(call.arguments());
      if (mode && mode->rfind("fail", 0) == 0) privacy_failure_ = *mode;
      if (mode && *mode == "clear") privacy_failure_.clear();
      const DWORD before = GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS);
      if (mode && *mode == "probe") {
        for (int i = 0; i < 20; ++i) {
          SendMessage(GetHandle(), WM_DWMSENDICONICTHUMBNAIL, 0, MAKELPARAM(80, 128));
          SendMessage(GetHandle(), WM_DWMSENDICONICLIVEPREVIEWBITMAP, 0, 0);
        }
      }

      result->Success(flutter::EncodableValue(flutter::EncodableMap{
        {flutter::EncodableValue("sensitive"), flutter::EncodableValue(sensitive_)},
        {flutter::EncodableValue("baselineReady"), flutter::EncodableValue(baseline_ready_)},
        {flutter::EncodableValue("force"), baseline_ready_ ? flutter::EncodableValue(force_value_ != FALSE) : flutter::EncodableValue()},
        {flutter::EncodableValue("bitmap"), baseline_ready_ ? flutter::EncodableValue(bitmap_value_ != FALSE) : flutter::EncodableValue()},
        {flutter::EncodableValue("attributesOk"), flutter::EncodableValue(SUCCEEDED(force_result_) && SUCCEEDED(bitmap_result_))},
        {flutter::EncodableValue("thumbnailOk"), flutter::EncodableValue(SUCCEEDED(thumbnail_result_))},
        {flutter::EncodableValue("previewOk"), flutter::EncodableValue(SUCCEEDED(preview_result_))},
        {flutter::EncodableValue("forceResult"), flutter::EncodableValue(static_cast<int>(force_result_))},
        {flutter::EncodableValue("bitmapResult"), flutter::EncodableValue(static_cast<int>(bitmap_result_))},
        {flutter::EncodableValue("thumbnailResult"), flutter::EncodableValue(static_cast<int>(thumbnail_result_))},
        {flutter::EncodableValue("previewResult"), flutter::EncodableValue(static_cast<int>(preview_result_))},
        {flutter::EncodableValue("gdiDelta"), flutter::EncodableValue(static_cast<int>(GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS)) - static_cast<int>(before))},
      })); return;
    }
#endif
    result->NotImplemented();
  });
  SetChildContent(flutter_controller_->view()->GetNativeWindow());

  flutter_controller_->engine()->SetNextFrameCallback([&]() {
    this->Show();
  });

  // Flutter can complete the first frame before the "show window" callback is
  // registered. The following call ensures a frame is pending to ensure the
  // window is shown. It is a no-op if the first frame hasn't completed yet.
  flutter_controller_->ForceRedraw();

  return true;
}

void FlutterWindow::OnDestroy() {
  SetSensitive(false);
  account_channel_.reset();
  privacy_events_.reset();
  if (flutter_controller_) {
    flutter_controller_ = nullptr;
  }

  Win32Window::OnDestroy();
}

LRESULT
FlutterWindow::MessageHandler(HWND hwnd, UINT const message,
                              WPARAM const wparam,
                              LPARAM const lparam) noexcept {
  if (sensitive_ && message == WM_DWMSENDICONICTHUMBNAIL) {
    thumbnail_result_ = DrawPrivateBitmap(hwnd, HIWORD(lparam), LOWORD(lparam), true);
    if (FAILED(thumbnail_result_)) PrivacyFailed();
    return 0;
  }
  if (sensitive_ && message == WM_DWMSENDICONICLIVEPREVIEWBITMAP) {
    RECT rect{};
    if (!GetClientRect(hwnd, &rect)) { PrivacyFailed(); return 0; }
    preview_result_ = DrawPrivateBitmap(hwnd, rect.right, rect.bottom, false);
    if (FAILED(preview_result_)) PrivacyFailed();
    return 0;
  }
  // Give Flutter, including plugins, an opportunity to handle window messages.
  if (flutter_controller_) {
    std::optional<LRESULT> result =
        flutter_controller_->HandleTopLevelWindowProc(hwnd, message, wparam,
                                                      lparam);
    if (result) {
      return *result;
    }
  }

  switch (message) {
    case WM_FONTCHANGE:
      flutter_controller_->engine()->ReloadSystemFonts();
      break;
  }

  return Win32Window::MessageHandler(hwnd, message, wparam, lparam);
}

HRESULT FlutterWindow::DrawPrivateBitmap(HWND window, int width, int height, bool thumbnail) {
#ifndef NDEBUG
  if (privacy_failure_ == "failBitmap") return E_FAIL;
#endif
  if (width <= 0 || height <= 0) return E_INVALIDARG;
  BITMAPINFO info{};
  info.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
  info.bmiHeader.biWidth = width;
  info.bmiHeader.biHeight = -height;
  info.bmiHeader.biPlanes = 1;
  info.bmiHeader.biBitCount = 32;
  info.bmiHeader.biCompression = BI_RGB;
  void* pixels = nullptr;
  HBITMAP bitmap = CreateDIBSection(nullptr, &info, DIB_RGB_COLORS, &pixels, nullptr, 0);
  if (!bitmap || !pixels) { if (bitmap) DeleteObject(bitmap); return E_OUTOFMEMORY; }
  std::fill_n(static_cast<DWORD*>(pixels), static_cast<size_t>(width) * height, 0xff203038);
  const HRESULT result = thumbnail ? DwmSetIconicThumbnail(window, bitmap, 0)
      : DwmSetIconicLivePreviewBitmap(window, bitmap, nullptr, 0);
  DeleteObject(bitmap); // DWM copies the bitmap; the caller retains ownership.
  return result;
}

void FlutterWindow::PrivacyFailed() {
  protection_ready_ = false;
  if (flutter_controller_) ShowWindow(flutter_controller_->view()->GetNativeWindow(), SW_HIDE);
  if (privacy_events_) privacy_events_->InvokeMethod("protectionFailed", nullptr);
}

bool FlutterWindow::SetSensitive(bool value) {
  const HWND window = GetHandle();
  if (!window) return !value;
  if (value) {
#ifndef NDEBUG
    if (privacy_failure_ == "fail") return false;
#endif
    if (!baseline_ready_) return false;
    if (sensitive_) return protection_ready_;
    if (!saved_attributes_) {
      previous_force_ = force_value_;
      previous_bitmap_ = bitmap_value_;
      saved_attributes_ = true;
    }
    const BOOL enabled = TRUE;
    sensitive_ = true;
    force_result_ = DwmSetWindowAttribute(window, DWMWA_FORCE_ICONIC_REPRESENTATION, &enabled, sizeof(enabled));
    if (SUCCEEDED(force_result_)) force_value_ = enabled;
#ifndef NDEBUG
    if (privacy_failure_ == "failPartial") bitmap_result_ = E_FAIL;
    else
#endif
    bitmap_result_ = DwmSetWindowAttribute(window, DWMWA_HAS_ICONIC_BITMAP, &enabled, sizeof(enabled));
    if (SUCCEEDED(bitmap_result_)) bitmap_value_ = enabled;
    const bool okay = SUCCEEDED(force_result_) && SUCCEEDED(bitmap_result_) && SUCCEEDED(DwmInvalidateIconicBitmaps(window));
    thumbnail_result_ = okay ? DrawPrivateBitmap(window, 1, 1, true) : E_FAIL;
    preview_result_ = okay ? DrawPrivateBitmap(window, 1, 1, false) : E_FAIL;
    if (okay && SUCCEEDED(thumbnail_result_) && SUCCEEDED(preview_result_)) {
      protection_ready_ = true;
      return true;
    }
    SetSensitive(false);
    return false;
  }
  protection_ready_ = false;
  if (!saved_attributes_) return true;
  force_result_ = DwmSetWindowAttribute(window, DWMWA_FORCE_ICONIC_REPRESENTATION, &previous_force_, sizeof(previous_force_));
  if (SUCCEEDED(force_result_)) force_value_ = previous_force_;
  bitmap_result_ = DwmSetWindowAttribute(window, DWMWA_HAS_ICONIC_BITMAP, &previous_bitmap_, sizeof(previous_bitmap_));
  if (SUCCEEDED(bitmap_result_)) bitmap_value_ = previous_bitmap_;
  HRESULT invalidated = DwmInvalidateIconicBitmaps(window);
#ifndef NDEBUG
  if (privacy_failure_ == "failRestore") invalidated = E_FAIL;
#endif
  if (FAILED(force_result_) || FAILED(bitmap_result_) || FAILED(invalidated)) { PrivacyFailed(); return false; }
  sensitive_ = false;
  saved_attributes_ = false;
  if (flutter_controller_) ShowWindow(flutter_controller_->view()->GetNativeWindow(), SW_SHOW);
  return true;
}

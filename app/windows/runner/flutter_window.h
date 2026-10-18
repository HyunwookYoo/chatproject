#ifndef RUNNER_FLUTTER_WINDOW_H_
#define RUNNER_FLUTTER_WINDOW_H_

#include <flutter/dart_project.h>
#include <flutter/flutter_view_controller.h>

#include <memory>
#include <flutter/method_channel.h>
#include <flutter/encodable_value.h>

#include "win32_window.h"

// A window that does nothing but host a Flutter view.
class FlutterWindow : public Win32Window {
 public:
  // Creates a new FlutterWindow hosting a Flutter view running |project|.
  explicit FlutterWindow(const flutter::DartProject& project);
  virtual ~FlutterWindow();

 protected:
  // Win32Window:
  bool OnCreate() override;
  void OnDestroy() override;
  LRESULT MessageHandler(HWND window, UINT const message, WPARAM const wparam,
                         LPARAM const lparam) noexcept override;

 private:
  bool SetSensitive(bool value);
  HRESULT DrawPrivateBitmap(HWND window, int width, int height, bool thumbnail);
  void PrivacyFailed();
  bool sensitive_ = false;
  bool protection_ready_ = false;
  bool saved_attributes_ = false;
  bool baseline_ready_ = false;
  BOOL force_value_ = FALSE;
  BOOL bitmap_value_ = FALSE;
  HRESULT force_result_ = E_PENDING;
  HRESULT bitmap_result_ = E_PENDING;
  BOOL previous_force_ = FALSE;
  BOOL previous_bitmap_ = FALSE;
  HRESULT thumbnail_result_ = E_PENDING;
  HRESULT preview_result_ = E_PENDING;
#ifndef NDEBUG
  std::string privacy_failure_;
#endif
  std::unique_ptr<flutter::MethodChannel<flutter::EncodableValue>> account_channel_;
  std::unique_ptr<flutter::MethodChannel<flutter::EncodableValue>> privacy_events_;
  // The project to run.
  flutter::DartProject project_;

  // The Flutter instance hosted by this window.
  std::unique_ptr<flutter::FlutterViewController> flutter_controller_;
};

#endif  // RUNNER_FLUTTER_WINDOW_H_

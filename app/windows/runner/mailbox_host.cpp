#include "mailbox_host.h"

#include <flutter/method_result_functions.h>
#include <flutter/standard_method_codec.h>
#include <shellapi.h>
#include <windowsx.h>

#include <cstdint>
#include <cwchar>

#include "mailbox_policy.h"
#include "resource.h"

namespace {

constexpr UINT kTrayMessage = WM_APP + 1;
constexpr UINT kTrayIconId = 1;
constexpr UINT_PTR kTrayStartTimer = 0x4D42;
// How long a --tray start waits for Dart to report the mailbox setting before
// it shows the window as if the mailbox were off.
constexpr UINT kTrayStartTimeoutMs = 15000;
constexpr UINT kMenuOpen = 1;
constexpr UINT kMenuQuit = 2;
constexpr wchar_t kRunKey[] = L"Software\\Microsoft\\Windows\\CurrentVersion\\Run";
constexpr wchar_t kRunValue[] = L"ChatProject";

std::wstring Utf16FromUtf8(const std::string& utf8) {
  if (utf8.empty()) return std::wstring();
  const int length = MultiByteToWideChar(CP_UTF8, 0, utf8.data(),
                                         static_cast<int>(utf8.size()), nullptr, 0);
  if (length <= 0) return std::wstring();
  std::wstring utf16(length, L'\0');
  MultiByteToWideChar(CP_UTF8, 0, utf8.data(), static_cast<int>(utf8.size()),
                      utf16.data(), length);
  return utf16;
}

#ifndef NDEBUG
std::string Utf8FromUtf16(const std::wstring& utf16) {
  if (utf16.empty()) return std::string();
  const int length = WideCharToMultiByte(CP_UTF8, 0, utf16.data(),
                                         static_cast<int>(utf16.size()), nullptr, 0,
                                         nullptr, nullptr);
  if (length <= 0) return std::string();
  std::string utf8(length, '\0');
  WideCharToMultiByte(CP_UTF8, 0, utf16.data(), static_cast<int>(utf16.size()),
                      utf8.data(), length, nullptr, nullptr);
  return utf8;
}
#endif

std::optional<int> IntValue(const flutter::EncodableValue* value) {
  if (!value) return std::nullopt;
  if (const auto* narrow = std::get_if<int32_t>(value)) return *narrow;
  if (const auto* wide = std::get_if<int64_t>(value)) {
    if (*wide >= INT32_MIN && *wide <= INT32_MAX) return static_cast<int>(*wide);
  }
  return std::nullopt;
}

const flutter::EncodableValue* Field(const flutter::EncodableMap& map,
                                     const char* key) {
  const auto found = map.find(flutter::EncodableValue(key));
  return found == map.end() ? nullptr : &found->second;
}

std::wstring AutostartCommand() {
  std::wstring path(MAX_PATH, L'\0');
  for (;;) {
    const DWORD length = GetModuleFileNameW(nullptr, path.data(),
                                            static_cast<DWORD>(path.size()));
    if (length == 0) return std::wstring();
    if (length < path.size()) {
      path.resize(length);
      break;
    }
    path.resize(path.size() * 2);
  }
  return L"\"" + path + L"\" --tray";
}

std::optional<std::wstring> RunValue() {
  DWORD size = 0;
  if (RegGetValueW(HKEY_CURRENT_USER, kRunKey, kRunValue, RRF_RT_REG_SZ, nullptr,
                   nullptr, &size) != ERROR_SUCCESS) {
    return std::nullopt;
  }
  std::wstring value(size / sizeof(wchar_t), L'\0');
  if (RegGetValueW(HKEY_CURRENT_USER, kRunKey, kRunValue, RRF_RT_REG_SZ, nullptr,
                   value.data(), &size) != ERROR_SUCCESS) {
    return std::nullopt;
  }
  value.resize(wcsnlen(value.c_str(), value.size()));
  return value;
}

HICON AppIcon(int metric_x, int metric_y) {
  return static_cast<HICON>(LoadImageW(GetModuleHandle(nullptr),
                                       MAKEINTRESOURCEW(IDI_APP_ICON), IMAGE_ICON,
                                       GetSystemMetrics(metric_x),
                                       GetSystemMetrics(metric_y), LR_SHARED));
}

}  // namespace

MailboxHost::MailboxHost(HWND window, flutter::BinaryMessenger* messenger)
    : window_(window),
      channel_(std::make_unique<flutter::MethodChannel<flutter::EncodableValue>>(
          messenger, "chat_app/mailbox",
          &flutter::StandardMethodCodec::GetInstance())),
      taskbar_created_(RegisterWindowMessageW(L"TaskbarCreated")) {
  channel_->SetMethodCallHandler([this](const auto& call, auto result) {
    HandleCall(call, std::move(result));
  });
}

MailboxHost::~MailboxHost() {
  channel_->SetMethodCallHandler(nullptr);
  KillTimer(window_, kTrayStartTimer);
  if (icon_added_) {
    NOTIFYICONDATAW data{sizeof(data)};
    data.hWnd = window_;
    data.uID = kTrayIconId;
    Shell_NotifyIconW(NIM_DELETE, &data);
  }
  if (power_request_ != INVALID_HANDLE_VALUE) {
    if (power_held_) PowerClearRequest(power_request_, PowerRequestSystemRequired);
    CloseHandle(power_request_);
  }
}

void MailboxHost::HandleCall(
    const flutter::MethodCall<flutter::EncodableValue>& call,
    std::unique_ptr<flutter::MethodResult<flutter::EncodableValue>> result) {
  const std::string& method = call.method_name();
  const flutter::EncodableValue* arguments = call.arguments();
  if (method == "setMailbox" || method == "setAutostart") {
    const auto* on = std::get_if<bool>(arguments);
    if (!on) { result->Error("invalid_argument"); return; }
    if (method == "setMailbox") {
      SetMailbox(*on);
      result->Success(flutter::EncodableValue(true));
    } else {
      result->Success(flutter::EncodableValue(SetAutostart(*on)));
    }
    return;
  }
  if (method == "autostart") {
    result->Success(flutter::EncodableValue(Autostart()));
    return;
  }
  if (method == "notify") {
    const auto* map = std::get_if<flutter::EncodableMap>(arguments);
    const auto id = map ? IntValue(Field(*map, "id")) : std::nullopt;
    const auto* title = map ? std::get_if<std::string>(Field(*map, "title")) : nullptr;
    const auto* body = map ? std::get_if<std::string>(Field(*map, "body")) : nullptr;
    if (!id || !title || !body) { result->Error("invalid_argument"); return; }
    Notify(*id, Utf16FromUtf8(*title), Utf16FromUtf8(*body));
    result->Success();
    return;
  }
  if (method == "cancelNotification") {
    const auto id = IntValue(arguments);
    if (!id) { result->Error("invalid_argument"); return; }
    Cancel(*id);
    result->Success();
    return;
  }
  if (method == "quit") {
    result->Success();
    PostQuitMessage(0);
    return;
  }
#ifndef NDEBUG
  if (method == "mailboxTest") {
    const auto* map = std::get_if<flutter::EncodableMap>(arguments);
    const flutter::EncodableMap empty;
    if (!map) map = &empty;
    const auto ac = IntValue(Field(*map, "ac"));
    const auto battery = IntValue(Field(*map, "battery"));
    SYSTEM_POWER_STATUS status{};
    if (ac && battery) {
      status.ACLineStatus = static_cast<BYTE>(*ac);
      status.BatteryFlag = static_cast<BYTE>(*battery);
    } else if (!GetSystemPowerStatus(&status)) {
      result->Error("power_status_unavailable");
      return;
    }
    // Overrides go through the same path as a real power broadcast.
    UpdatePower(status.ACLineStatus, status.BatteryFlag);
    const bool hold = ShouldHoldSystemRequired(mailbox_on_, status.ACLineStatus,
                                               status.BatteryFlag);
    const auto* open = std::get_if<bool>(Field(*map, "open"));
    if (open && *open) Open();
    const auto* close = std::get_if<bool>(Field(*map, "close"));
    if (close && *close) {
      if (close_probe_) { result->Error("close_probe_pending"); return; }
      // A real WM_CLOSE. It reaches OnCloseRequested after Flutter's exit
      // request, which answers this call instead of destroying the window.
      close_probe_ = std::move(result);
      PostMessage(window_, WM_CLOSE, 0, 0);
      return;
    }
    result->Success(ProbeState(hold, nullptr));
    return;
  }
#endif
  result->NotImplemented();
}

void MailboxHost::StartInTray() {
  tray_start_pending_ = true;
  UpdateIcon();
  SetTimer(window_, kTrayStartTimer, kTrayStartTimeoutMs, nullptr);
}

void MailboxHost::SettleTrayStart() {
  if (!tray_start_pending_) return;
  tray_start_pending_ = false;
  KillTimer(window_, kTrayStartTimer);
  // Never leave a running app with no window and no tray icon.
  if (!mailbox_on_ || !icon_added_) ShowWindow(window_, SW_SHOW);
}

void MailboxHost::SetMailbox(bool on) {
  mailbox_on_ = on;
  SettleTrayStart();
  UpdateIcon();
  OnPowerBroadcast();
}

void MailboxHost::OnPowerBroadcast() {
  SYSTEM_POWER_STATUS status{};
  // Without a status, treat the PC as a desktop of unknown AC state.
  if (!GetSystemPowerStatus(&status)) {
    status.ACLineStatus = 255;
    status.BatteryFlag = 255;
  }
  UpdatePower(status.ACLineStatus, status.BatteryFlag);
}

void MailboxHost::UpdatePower(BYTE ac_line_status, BYTE battery_flag) {
  const bool hold = ShouldHoldSystemRequired(mailbox_on_, ac_line_status, battery_flag);
  if (hold == power_held_) return;
  if (!hold) {
    PowerClearRequest(power_request_, PowerRequestSystemRequired);
    power_held_ = false;
    return;
  }
  if (power_request_ == INVALID_HANDLE_VALUE) {
    REASON_CONTEXT reason{};
    reason.Version = POWER_REQUEST_CONTEXT_VERSION;
    reason.Flags = POWER_REQUEST_CONTEXT_SIMPLE_STRING;
    reason.Reason.SimpleReasonString =
        const_cast<LPWSTR>(L"ChatProject mailbox is receiving messages");
    power_request_ = PowerCreateRequest(&reason);
    if (power_request_ == INVALID_HANDLE_VALUE) return;
  }
  // Only the system stays awake; the display may still turn off.
  power_held_ = PowerSetRequest(power_request_, PowerRequestSystemRequired) != FALSE;
}

bool MailboxHost::SetAutostart(bool on) {
  if (!on) {
    const LSTATUS status = RegDeleteKeyValueW(HKEY_CURRENT_USER, kRunKey, kRunValue);
    return status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND;
  }
  const std::wstring command = AutostartCommand();
  if (command.empty()) return false;
  return RegSetKeyValueW(HKEY_CURRENT_USER, kRunKey, kRunValue, REG_SZ,
                         command.c_str(),
                         static_cast<DWORD>((command.size() + 1) * sizeof(wchar_t))) ==
         ERROR_SUCCESS;
}

bool MailboxHost::Autostart() { return RunValue().has_value(); }

void MailboxHost::Notify(int id, const std::wstring& title, const std::wstring& body) {
  balloon_id_ = id;
  UpdateIcon();
  if (!icon_added_) return;
  NOTIFYICONDATAW data{sizeof(data)};
  data.hWnd = window_;
  data.uID = kTrayIconId;
  data.uFlags = NIF_INFO;
  wcsncpy_s(data.szInfoTitle, title.c_str(), _TRUNCATE);
  wcsncpy_s(data.szInfo, body.empty() ? L" " : body.c_str(), _TRUNCATE);
  data.dwInfoFlags = NIIF_USER | NIIF_LARGE_ICON;
  data.hBalloonIcon = AppIcon(SM_CXICON, SM_CYICON);
  Shell_NotifyIconW(NIM_MODIFY, &data);
}

void MailboxHost::Cancel(int id) {
  if (balloon_id_ != id) return;
  balloon_id_.reset();
  if (icon_added_) {
    // An empty text removes the balloon that is showing.
    NOTIFYICONDATAW data{sizeof(data)};
    data.hWnd = window_;
    data.uID = kTrayIconId;
    data.uFlags = NIF_INFO;
    Shell_NotifyIconW(NIM_MODIFY, &data);
  }
  UpdateIcon();
}

bool MailboxHost::OnCloseRequested() {
  // Hide only behind an icon the user can reopen the window from.
  const CloseAction action = OnClose(mailbox_on_ && icon_added_);
  if (action == CloseAction::HideToTray) ShowWindow(window_, SW_HIDE);
#ifndef NDEBUG
  if (close_probe_) {
    SYSTEM_POWER_STATUS status{};
    GetSystemPowerStatus(&status);
    auto probe = std::move(close_probe_);
    probe->Success(ProbeState(
        ShouldHoldSystemRequired(mailbox_on_, status.ACLineStatus, status.BatteryFlag),
        action == CloseAction::HideToTray ? "hide" : "destroy"));
    return false;
  }
#endif
  return action == CloseAction::Quit;
}

std::optional<LRESULT> MailboxHost::HandleMessage(UINT message, WPARAM wparam,
                                                  LPARAM lparam) {
  if (message == WM_POWERBROADCAST) {
    if (wparam == PBT_APMPOWERSTATUSCHANGE) OnPowerBroadcast();
    return std::nullopt;
  }
  if (message == WM_TIMER && wparam == kTrayStartTimer) {
    SettleTrayStart();
    UpdateIcon();
    return 0;
  }
  if (taskbar_created_ != 0 && message == taskbar_created_) {
    // Explorer restarted and dropped every tray icon.
    icon_added_ = false;
    UpdateIcon();
    return 0;
  }
  if (message != kTrayMessage) return std::nullopt;
  switch (LOWORD(lparam)) {
    case WM_CONTEXTMENU:
      ShowMenu(GET_X_LPARAM(wparam), GET_Y_LPARAM(wparam));
      break;
    case WM_LBUTTONDBLCLK:
      Open();
      break;
    case NIN_BALLOONUSERCLICK: {
      const auto id = balloon_id_;
      balloon_id_.reset();
      UpdateIcon();
      Open();
      if (id) channel_->InvokeMethod(
          "openConversation", std::make_unique<flutter::EncodableValue>(*id));
      break;
    }
    case NIN_BALLOONTIMEOUT:
      balloon_id_.reset();
      UpdateIcon();
      break;
  }
  return 0;
}

void MailboxHost::Open() {
  ShowWindow(window_, IsIconic(window_) ? SW_RESTORE : SW_SHOW);
  SetForegroundWindow(window_);
}

void MailboxHost::RequestExit() {
  // Dart flushes the core and answers with `quit`. A Dart side that does not
  // handle the request must not leave the app impossible to quit.
  channel_->InvokeMethod(
      "exitRequested", nullptr,
      std::make_unique<flutter::MethodResultFunctions<flutter::EncodableValue>>(
          nullptr,
          [](const std::string&, const std::string&, const flutter::EncodableValue*) {
            PostQuitMessage(0);
          },
          []() { PostQuitMessage(0); }));
}

void MailboxHost::ShowMenu(int x, int y) {
  HMENU menu = CreatePopupMenu();
  if (!menu) return;
  AppendMenuW(menu, MF_STRING, kMenuOpen, L"\uC5F4\uAE30");  // "Open"
  AppendMenuW(menu, MF_STRING, kMenuQuit,
              L"\uC644\uC804\uD788 \uC885\uB8CC");  // "Quit completely"
  // The menu closes on an outside click only if its owner is foreground.
  SetForegroundWindow(window_);
  const UINT align =
      GetSystemMetrics(SM_MENUDROPALIGNMENT) ? TPM_RIGHTALIGN : TPM_LEFTALIGN;
  const UINT command = static_cast<UINT>(TrackPopupMenuEx(
      menu, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | align, x, y, window_,
      nullptr));
  PostMessage(window_, WM_NULL, 0, 0);
  DestroyMenu(menu);
  if (command == kMenuOpen) Open();
  if (command == kMenuQuit) RequestExit();
}

void MailboxHost::UpdateIcon() {
  const bool needed = mailbox_on_ || tray_start_pending_ || balloon_id_.has_value();
  NOTIFYICONDATAW data{sizeof(data)};
  data.hWnd = window_;
  data.uID = kTrayIconId;
  if (needed && !icon_added_) {
    data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP;
    data.uCallbackMessage = kTrayMessage;
    data.hIcon = AppIcon(SM_CXSMICON, SM_CYSMICON);
    wcsncpy_s(data.szTip, L"ChatProject", _TRUNCATE);
    if (!Shell_NotifyIconW(NIM_ADD, &data)) return;
    icon_added_ = true;
    data.uVersion = NOTIFYICON_VERSION_4;
    Shell_NotifyIconW(NIM_SETVERSION, &data);
  } else if (!needed && icon_added_) {
    Shell_NotifyIconW(NIM_DELETE, &data);
    icon_added_ = false;
  }
}

#ifndef NDEBUG
flutter::EncodableValue MailboxHost::ProbeState(bool hold, const char* close_action) {
  const auto run = RunValue();
  return flutter::EncodableValue(flutter::EncodableMap{
      {flutter::EncodableValue("tray"), flutter::EncodableValue(icon_added_)},
      {flutter::EncodableValue("powerHeld"), flutter::EncodableValue(power_held_)},
      {flutter::EncodableValue("hold"), flutter::EncodableValue(hold)},
      {flutter::EncodableValue("autostart"),
       run ? flutter::EncodableValue(Utf8FromUtf16(*run)) : flutter::EncodableValue()},
      {flutter::EncodableValue("visible"),
       flutter::EncodableValue(IsWindowVisible(window_) != FALSE)},
      {flutter::EncodableValue("closeAction"),
       close_action ? flutter::EncodableValue(std::string(close_action))
                    : flutter::EncodableValue()},
  });
}
#endif

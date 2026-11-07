#ifndef RUNNER_MAILBOX_HOST_H_
#define RUNNER_MAILBOX_HOST_H_

#include <flutter/binary_messenger.h>
#include <flutter/encodable_value.h>
#include <flutter/method_channel.h>
#include <windows.h>

#include <memory>
#include <optional>
#include <string>

// The Windows side of a mailbox PC: the tray icon and its menu, closing to the
// tray, the idle-sleep power request, autostart and tray notifications, behind
// the `chat_app/mailbox` channel.
class MailboxHost {
 public:
  MailboxHost(HWND window, flutter::BinaryMessenger* messenger);
  ~MailboxHost();

  MailboxHost(const MailboxHost&) = delete;
  MailboxHost& operator=(const MailboxHost&) = delete;

  // Started with --tray. If the last setMailbox was on (remembered under
  // HKCU\Software\ChatProject), stay in the tray as a mailbox at once.
  // Otherwise stay hidden behind a tray icon until Dart reports the setting;
  // off, or no report in time, shows the window.
  void StartInTray();

  // Adds or removes the tray icon and re-evaluates the power request.
  void SetMailbox(bool on);
  // Re-evaluates the power request from the current power status.
  void OnPowerBroadcast();
  bool SetAutostart(bool on);
  bool Autostart();
  void Notify(int id, const std::wstring& title, const std::wstring& body);
  void Cancel(int id);

  // WM_CLOSE. Returns true when the window should be destroyed.
  bool OnCloseRequested();
  // Tray callbacks, TaskbarCreated, power broadcasts and the --tray timer.
  std::optional<LRESULT> HandleMessage(UINT message, WPARAM wparam,
                                       LPARAM lparam);

 private:
  void HandleCall(
      const flutter::MethodCall<flutter::EncodableValue>& call,
      std::unique_ptr<flutter::MethodResult<flutter::EncodableValue>> result);
  void Open();
  void RequestExit();
  void ShowMenu(int x, int y);
  void UpdateIcon();
  void UpdatePower(BYTE ac_line_status, BYTE battery_flag);
  void SettleTrayStart();
#ifndef NDEBUG
  flutter::EncodableValue ProbeState(bool hold, const char* close_action);
#endif

  HWND window_;
  std::unique_ptr<flutter::MethodChannel<flutter::EncodableValue>> channel_;
  UINT taskbar_created_ = 0;
  bool mailbox_on_ = false;
  bool tray_start_pending_ = false;
  std::optional<int> balloon_id_;
  bool icon_added_ = false;
  HANDLE power_request_ = INVALID_HANDLE_VALUE;
  bool power_held_ = false;
#ifndef NDEBUG
  std::unique_ptr<flutter::MethodResult<flutter::EncodableValue>> close_probe_;
#endif
};

#endif  // RUNNER_MAILBOX_HOST_H_

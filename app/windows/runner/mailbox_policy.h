#ifndef RUNNER_MAILBOX_POLICY_H_
#define RUNNER_MAILBOX_POLICY_H_

#include <windows.h>

// Hold the system awake while the mailbox is on, but only on AC power. An
// unknown AC status (255) or no system battery (BatteryFlag 128) is a desktop,
// which also holds. The display may still turn off.
inline bool ShouldHoldSystemRequired(bool mailbox_on, BYTE ac_line_status,
                                     BYTE battery_flag) {
  return mailbox_on && (ac_line_status == 1 || ac_line_status == 255 ||
                        (battery_flag & 128) != 0);
}

enum class CloseAction { HideToTray, Quit };

// Closing the window keeps a mailbox PC running in the tray; otherwise it quits.
inline CloseAction OnClose(bool mailbox_on) {
  return mailbox_on ? CloseAction::HideToTray : CloseAction::Quit;
}

#endif  // RUNNER_MAILBOX_POLICY_H_

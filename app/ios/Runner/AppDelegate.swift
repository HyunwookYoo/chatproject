import BackgroundTasks
import Flutter
import UIKit
import UserNotifications

@main
@objc class AppDelegate: FlutterAppDelegate, FlutterImplicitEngineDelegate {
  static var invitationChannel: FlutterMethodChannel?
  static var pendingInvitation: String?
  static func receiveInvitation(_ url: URL) {
    let text = url.absoluteString
    guard text.hasPrefix("chatproject:identity-invite/") else { return }
    pendingInvitation = text
    invitationChannel?.invokeMethod("identityInvite", arguments: text) { result in
      if result as? Bool == true, pendingInvitation == text {
        pendingInvitation = nil
      }
    }
  }
  private let push = PushHost()

  override func application(
    _ application: UIApplication,
    didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
  ) -> Bool {
    // Before the Flutter engine starts, so Dart cannot open account storage without the Keychain.
    _ = cp_key_register(cpKeyProtect, cpKeyUnprotect, cpKeyDelete)
    // BGTaskScheduler takes registrations only before launching ends.
    push.launch()
    return super.application(application, didFinishLaunchingWithOptions: launchOptions)
  }

  func didInitializeImplicitFlutterEngine(_ engineBridge: FlutterImplicitEngineBridge) {
    GeneratedPluginRegistrant.register(with: engineBridge.pluginRegistry)
    guard let registrar = engineBridge.pluginRegistry.registrar(forPlugin: "ChatProject") else { return }
    let accountChannel = FlutterMethodChannel(name: "chat_app/account_storage", binaryMessenger: registrar.messenger())
    Self.invitationChannel = accountChannel
    accountChannel.setMethodCallHandler { call, result in
      switch call.method {
      case "takeIdentityInvite":
        result(Self.pendingInvitation)
        Self.pendingInvitation = nil
      case "storageRoot":
        do {
          let args = call.arguments as? [String: Any]
          result(try AccountStorage.root(existing: args?["existing"] as? String))
        } catch { result(FlutterError(code: "storage_unavailable", message: "계정 저장 공간을 열 수 없어요.", details: nil)) }
      case "setSensitiveScreen":
        guard let sensitive = call.arguments as? Bool else {
          result(FlutterError(code: "invalid_argument", message: nil, details: nil)); return
        }
        SceneDelegate.setSensitiveScreen(sensitive)
        result(nil)
      default: result(FlutterMethodNotImplemented)
      }
    }
    push.attach(registrar.messenger())
  }

  override func application(
    _ application: UIApplication,
    didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data
  ) {
    push.registered(deviceToken)
    super.application(application, didRegisterForRemoteNotificationsWithDeviceToken: deviceToken)
  }
}

/// Pushes on the app side (M7 spec 5.1, 5.6, 8), over the channel `chat_app/push`.
///
/// Dart calls `requestPermission()` (alert permission, then APNs registration; true when
/// granted), `token()` (the APNs token in hex, or nil before iOS gave one), `appGroupDir()` (the
/// App Group folder: Dart passes it as `snapshot_dir`, and `<it>/push` is the inbox folder) and
/// `takeOpenedTag()` (the `t` of a notification tapped before Dart listened, once),
/// `permissionStatus()` ("granted", "denied", or "notDetermined" while neither iOS nor the
/// permission screen's "나중에" answered), `markPermissionAsked()` ("나중에") and
/// `openNotificationSettings()` (this app's page in Settings). Swift calls
/// `tokenChanged(hex)` when iOS gives a token and `notificationOpened(t)` on a tap; Dart answers
/// true when it took the tap.
///
/// The app shows its own texts while it is active, so a notification then goes to the list
/// only; it never posts one of its own. A `BGAppRefreshTask` renews the handles in the
/// background (`cp_push_renew`) at most every 12 hours.
final class PushHost: NSObject, UNUserNotificationCenterDelegate {
  static let renewTask = "dev.chatproject.chatapp.push-renew"
  private static let appGroup = "group.dev.chatproject.chatapp"
  /// The renewal's share of the about 30 seconds iOS gives a refresh task.
  private static let renewBudgetMs: UInt32 = 25_000
  private static let renewEvery: TimeInterval = 12 * 60 * 60
  /// Set by the permission screen's "나중에", so it is shown once (M7 spec 4).
  private static let permissionAsked = "notification_permission_asked"

  private var channel: FlutterMethodChannel?
  private var token: String?
  private var openedTag: String?

  func launch() {
    UNUserNotificationCenter.current().delegate = self
    BGTaskScheduler.shared.register(forTaskWithIdentifier: Self.renewTask, using: nil) { [weak self] task in
      self?.renew(task)
    }
    scheduleRenewal()
    // A token can change between launches; iOS hands out the current one without asking again.
    UNUserNotificationCenter.current().getNotificationSettings { settings in
      guard [.authorized, .provisional, .ephemeral].contains(settings.authorizationStatus) else { return }
      DispatchQueue.main.async { UIApplication.shared.registerForRemoteNotifications() }
    }
  }

  func attach(_ messenger: FlutterBinaryMessenger) {
    let channel = FlutterMethodChannel(name: "chat_app/push", binaryMessenger: messenger)
    self.channel = channel
    channel.setMethodCallHandler { [weak self] call, result in
      guard let self else { return result(FlutterMethodNotImplemented) }
      switch call.method {
      case "requestPermission":
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound, .badge]) { granted, _ in
          DispatchQueue.main.async {
            if granted { UIApplication.shared.registerForRemoteNotifications() }
            result(granted)
          }
        }
      case "permissionStatus":
        UNUserNotificationCenter.current().getNotificationSettings { settings in
          let status: String
          switch settings.authorizationStatus {
          case .authorized, .provisional, .ephemeral:
            status = "granted"
          case .notDetermined:
            status = UserDefaults.standard.bool(forKey: Self.permissionAsked) ? "denied" : "notDetermined"
          default:
            status = "denied"
          }
          DispatchQueue.main.async { result(status) }
        }
      case "markPermissionAsked":
        UserDefaults.standard.set(true, forKey: Self.permissionAsked)
        result(nil)
      case "openNotificationSettings":
        if let url = URL(string: UIApplication.openSettingsURLString) {
          UIApplication.shared.open(url)
        }
        result(nil)
      case "token":
        result(self.token)
      case "appGroupDir":
        if let dir = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: Self.appGroup) {
          result(dir.path)
        } else {
          result(FlutterError(code: "app_group_unavailable", message: nil, details: nil))
        }
      case "takeOpenedTag":
        result(self.openedTag)
        self.openedTag = nil
      #if DEBUG
      // Integration tests only: the data protection class of a file (spec 5.6).
      case "fileProtection":
        guard let path = call.arguments as? String,
              let attributes = try? FileManager.default.attributesOfItem(atPath: path)
        else { return result(nil) }
        let protection = attributes[.protectionKey]
        result((protection as? FileProtectionType)?.rawValue ?? (protection as? String) ?? "")
      #endif
      default:
        result(FlutterMethodNotImplemented)
      }
    }
  }

  func registered(_ deviceToken: Data) {
    let hex = deviceToken.map { String(format: "%02x", $0) }.joined()
    token = hex
    channel?.invokeMethod("tokenChanged", arguments: hex)
  }

  func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification,
                              withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void) {
    DispatchQueue.main.async {
      completionHandler(UIApplication.shared.applicationState == .active ? [.list] : [.banner, .list, .sound])
    }
  }

  func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
                              withCompletionHandler completionHandler: @escaping () -> Void) {
    let t = response.notification.request.content.userInfo["t"] as? String
    DispatchQueue.main.async {
      if response.actionIdentifier == UNNotificationDefaultActionIdentifier, let t {
        self.openedTag = t
        self.channel?.invokeMethod("notificationOpened", arguments: t) { [weak self] taken in
          if taken as? Bool == true, self?.openedTag == t { self?.openedTag = nil }
        }
      }
      completionHandler()
    }
  }

  private func scheduleRenewal() {
    let request = BGAppRefreshTaskRequest(identifier: Self.renewTask)
    request.earliestBeginDate = Date(timeIntervalSinceNow: Self.renewEvery)
    try? BGTaskScheduler.shared.submit(request)
  }

  /// Renews the handles that are due, within the task's time (`cp_push_renew` is bounded by
  /// `renewBudgetMs`), and asks for the next run first.
  private func renew(_ task: BGTask) {
    scheduleRenewal()
    let lock = NSLock()
    var finished = false
    let finish = { (success: Bool) in
      lock.lock()
      defer { lock.unlock() }
      guard !finished else { return }
      finished = true
      task.setTaskCompleted(success: success)
    }
    task.expirationHandler = { finish(false) }
    DispatchQueue.global(qos: .utility).async {
      // The folder Dart starts the core in (FrbCoreClient.start): an older one if it holds an account.
      let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
      let existing = support?.appendingPathComponent("core", isDirectory: true).path
      guard let root = try? AccountStorage.root(existing: existing) else { return finish(false) }
      let status = root.withCString { cp_push_renew($0, Self.renewBudgetMs) }
      finish(status == 0)
    }
  }
}

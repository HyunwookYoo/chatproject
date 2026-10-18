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
  /// Hex APNs device token for the M1b probe screen, set once iOS registers the app.
  private var apnsToken: String?
  /// Why iOS could not register the app, for the probe screen: TestFlight has no console to read it from.
  private var registrationError: String?
  /// The `requestPush` call that waits for iOS to answer the registration.
  private var pendingPush: FlutterResult?

  override func application(
    _ application: UIApplication,
    didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
  ) -> Bool {
    return super.application(application, didFinishLaunchingWithOptions: launchOptions)
  }

  func didInitializeImplicitFlutterEngine(_ engineBridge: FlutterImplicitEngineBridge) {
    GeneratedPluginRegistrant.register(with: engineBridge.pluginRegistry)
    guard let registrar = engineBridge.pluginRegistry.registrar(forPlugin: "ChatProbe") else { return }
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
    let channel = FlutterMethodChannel(name: "chat_app/ios_probe", binaryMessenger: registrar.messenger())
    channel.setMethodCallHandler { [weak self] call, result in
      switch call.method {
      case "requestPush":
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound]) { granted, _ in
          DispatchQueue.main.async {
            guard granted, let self else { return result(granted) }
            self.registerForPush(result)
          }
        }
      case "deviceToken":
        result(self?.apnsToken)
      case "registrationError":
        result(self?.registrationError)
      default:
        result(FlutterMethodNotImplemented)
      }
    }
  }

  /// Asks iOS for a device token. `result` gets true once the token or the failure has come back, or after
  /// 10 seconds, so the probe screen can read `deviceToken` and `registrationError` right after.
  private func registerForPush(_ result: @escaping FlutterResult) {
    finishPush()  // a second call must not leave the first one unanswered
    pendingPush = result
    registrationError = nil
    UIApplication.shared.registerForRemoteNotifications()
    DispatchQueue.main.asyncAfter(deadline: .now() + 10) { [weak self] in self?.finishPush() }
  }

  private func finishPush() {
    guard let result = pendingPush else { return }
    pendingPush = nil
    result(true)
  }

  override func application(
    _ application: UIApplication,
    didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data
  ) {
    apnsToken = deviceToken.map { String(format: "%02x", $0) }.joined()
    registrationError = nil
    finishPush()
    super.application(application, didRegisterForRemoteNotificationsWithDeviceToken: deviceToken)
  }

  override func application(
    _ application: UIApplication,
    didFailToRegisterForRemoteNotificationsWithError error: Error
  ) {
    let e = error as NSError
    registrationError = "\(e.localizedDescription) (\(e.domain) \(e.code))"
    finishPush()
    super.application(application, didFailToRegisterForRemoteNotificationsWithError: error)
  }
}

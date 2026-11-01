import Flutter
import UIKit

class SceneDelegate: FlutterSceneDelegate {
  override func scene(_ scene: UIScene, willConnectTo session: UISceneSession, options connectionOptions: UIScene.ConnectionOptions) {
    super.scene(scene, willConnectTo: session, options: connectionOptions)
    for context in connectionOptions.urlContexts { AppDelegate.receiveInvitation(context.url) }
  }
  override func scene(_ scene: UIScene, openURLContexts URLContexts: Set<UIOpenURLContext>) {
    for context in URLContexts { AppDelegate.receiveInvitation(context.url) }
  }
  private static var sensitive = false
  private var privacyOverlay: UIView?
  private var lifecycleGeneration: UInt64 = 0
  private var backgroundTask: UIBackgroundTaskIdentifier = .invalid

  private func endBackgroundTask() {
    if backgroundTask != .invalid {
      UIApplication.shared.endBackgroundTask(backgroundTask)
      backgroundTask = .invalid
    }
  }

  private func updateAccountLifecycle(suspended: Bool) {
    lifecycleGeneration &+= 1
    let generation = lifecycleGeneration
    endBackgroundTask()
    // The ABI gates synchronously and queues IO on the existing Rust actor.
    // No semaphore, sync dispatch, or filesystem operation blocks UIKit here.
    let status = suspended ? cp_account_suspend() : cp_account_resume()
    guard status == 1 else { return }
    if suspended {
      backgroundTask = UIApplication.shared.beginBackgroundTask(withName: "Account suspend") { [weak self] in
        guard let self, self.lifecycleGeneration == generation else { return }
        self.lifecycleGeneration &+= 1
        self.endBackgroundTask()
        // Rust remains gated if the OS expires this finite drain interval.
      }
    }
    pollAccountLifecycle(suspended: suspended, generation: generation,
                         deadline: ProcessInfo.processInfo.systemUptime + 2.5)
  }

  private func pollAccountLifecycle(suspended: Bool, generation: UInt64, deadline: TimeInterval) {
    DispatchQueue.main.asyncAfter(deadline: .now() + 0.05) { [weak self] in
      guard let self, self.lifecycleGeneration == generation else { return }
      let status = suspended ? cp_account_suspend() : cp_account_resume()
      guard status == 1, ProcessInfo.processInfo.systemUptime < deadline else {
        self.endBackgroundTask()
        return
      }
      self.pollAccountLifecycle(suspended: suspended, generation: generation, deadline: deadline)
    }
  }

  static func setSensitiveScreen(_ sensitive: Bool) {
    Self.sensitive = sensitive
    for scene in UIApplication.shared.connectedScenes {
      if let delegate = scene.delegate as? SceneDelegate {
        delegate.updateOverlay(hidden: scene.activationState == .foregroundActive)
      }
    }
  }

  private func updateOverlay(hidden: Bool) {
    privacyOverlay?.removeFromSuperview()
    privacyOverlay = nil
    guard Self.sensitive, !hidden, let window else { return }
    let overlay = UIView(frame: window.bounds)
    overlay.backgroundColor = .systemBackground
    overlay.autoresizingMask = [.flexibleWidth, .flexibleHeight]
    window.addSubview(overlay)
    privacyOverlay = overlay
  }

  override func sceneWillResignActive(_ scene: UIScene) {
    updateOverlay(hidden: false)
    updateAccountLifecycle(suspended: true)
    super.sceneWillResignActive(scene)
  }

  override func sceneDidBecomeActive(_ scene: UIScene) {
    super.sceneDidBecomeActive(scene)
    updateAccountLifecycle(suspended: false)
    updateOverlay(hidden: true)
  }
}

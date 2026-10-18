import Flutter
import UIKit

class SceneDelegate: FlutterSceneDelegate {
  private static var sensitive = false
  private var privacyOverlay: UIView?

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
    super.sceneWillResignActive(scene)
  }

  override func sceneDidBecomeActive(_ scene: UIScene) {
    super.sceneDidBecomeActive(scene)
    updateOverlay(hidden: true)
  }
}

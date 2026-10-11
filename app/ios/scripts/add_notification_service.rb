# frozen_string_literal: true

# Adds the NotificationService app extension target to Runner.xcodeproj (M1b).
# There is no Mac, so this script is the source of truth: CI runs it before every iOS build.
# Running it twice changes nothing ("already up to date").
#
#   gem install xcodeproj -v 1.28.1 --no-document
#   ruby app/ios/scripts/add_notification_service.rb app/ios/Runner.xcodeproj
gem 'xcodeproj', '1.28.1'
require 'xcodeproj'

APP_ID = 'dev.chatproject.chatapp'
NSE = 'NotificationService'
CONFIGS = %w[Debug Release Profile].freeze

project_path = ARGV.fetch(0)
project = Xcodeproj::Project.open(project_path)
deployment_target = project.build_configuration_list['Release'].build_settings['IPHONEOS_DEPLOYMENT_TARGET']
changed = false

# Compare in the form xcodeproj writes (arrays joined by spaces).
set = lambda do |config, key, value|
  flat = ->(v) { v.is_a?(Array) ? v.join(' ') : v }
  next if flat.(config.build_settings[key]) == flat.(value)

  config.build_settings[key] = value
  changed = true
end
file_ref = ->(group, path) { group.files.find { |f| f.path == path } || group.new_file(path) }

runner = project.native_targets.find { |t| t.name == 'Runner' } or abort 'no Runner target'

# Files of app/ios/NotificationService.
group = project.main_group[NSE] || project.main_group.new_group(NSE, NSE)
sources = ["#{NSE}.swift", 'Generated/chat_nse.swift'].map { |p| file_ref.(group, p) }
%W[Info.plist #{NSE}.entitlements #{NSE}-Bridging-Header.h Generated/chat_nseFFI.h].each { |p| file_ref.(group, p) }
state = file_ref.(group, 'Fixture/nse_state.bin')

# The extension target.
nse = project.native_targets.find { |t| t.name == NSE } ||
      project.new_target(:app_extension, NSE, :ios, deployment_target, nil, :swift)
CONFIGS.each { |name| nse.add_build_configuration(name, name == 'Debug' ? :debug : :release) }
nse.add_file_references(sources)
nse.add_resources([state])

# Build settings. Flutter/Generated.xcconfig supplies FLUTTER_BUILD_NAME/NUMBER for Info.plist.
# Runner's own Debug/Release.xcconfig is not used here: Flutter prepends the CocoaPods include to
# those files, which would pass -framework chat_ffi and the Pods runpaths to the extension.
generated = project.files.find { |f| f.path == 'Flutter/Generated.xcconfig' } or abort 'no Flutter/Generated.xcconfig'
CONFIGS.each do |name|
  config = nse.build_configuration_list[name]
  if config.base_configuration_reference != generated
    config.base_configuration_reference = generated
    changed = true
  end
  {
    'PRODUCT_NAME' => '$(TARGET_NAME)',
    'PRODUCT_BUNDLE_IDENTIFIER' => "#{APP_ID}.#{NSE}",
    'INFOPLIST_FILE' => "#{NSE}/Info.plist",
    'CODE_SIGN_ENTITLEMENTS' => "#{NSE}/#{NSE}.entitlements",
    'SWIFT_VERSION' => '5.0',
    'TARGETED_DEVICE_FAMILY' => '1,2',
    'IPHONEOS_DEPLOYMENT_TARGET' => deployment_target,
    'SKIP_INSTALL' => 'YES',
    'SWIFT_OBJC_BRIDGING_HEADER' => "#{NSE}/#{NSE}-Bridging-Header.h",
    'LIBRARY_SEARCH_PATHS[sdk=iphoneos*]' => '$(SRCROOT)/../../target/aarch64-apple-ios/release',
    'LIBRARY_SEARCH_PATHS[sdk=iphonesimulator*]' => '$(SRCROOT)/../../target/aarch64-apple-ios-sim/release',
    'OTHER_LDFLAGS' => ['$(inherited)', '-lchat_nse'],
    'EXCLUDED_ARCHS[sdk=iphonesimulator*]' => '$(inherited) x86_64' # the Rust simulator library is arm64 only
  }.each { |key, value| set.(config, key, value) }
end

# Embed the extension in Runner and build it first.
embed = runner.copy_files_build_phases.find { |p| p.name == 'Embed Foundation Extensions' } ||
        runner.new_copy_files_build_phase('Embed Foundation Extensions')
embed.symbol_dst_subfolder_spec = :plug_ins # 13
embed.dst_path = ''
embed.add_file_reference(nse.product_reference, true).settings = { 'ATTRIBUTES' => ['RemoveHeadersOnCopy'] }
runner.add_dependency(nse)
# Flutter's "Cycle inside Runner" fix: Embed Foundation Extensions must come before Run Script.
phases = runner.build_phases
run_script = phases.find { |p| p.is_a?(Xcodeproj::Project::Object::PBXShellScriptBuildPhase) && p.name == 'Run Script' } or
  abort 'no Run Script phase'
phases.move(embed, phases.index(run_script)) if phases.index(embed) > phases.index(run_script)

# Runner: App Group + push entitlements.
file_ref.(project.main_group['Runner'], 'Runner.entitlements')
runner.build_configurations.each do |config|
  set.(config, 'PRODUCT_BUNDLE_IDENTIFIER', APP_ID)
  set.(config, 'CODE_SIGN_ENTITLEMENTS', 'Runner/Runner.entitlements')
end

# Signing (M1b plan, decision 3). Debug and Profile sign automatically (simulator builds need
# no profile). Release signs manually with the App Store profiles that the testflight workflow
# installs; without Manual, archive falls back to automatic signing and fails with "No Accounts".
TEAM = '9J2FNH63M2'
PROFILES = { 'Runner' => 'ChatProject App Store', NSE => 'ChatProject NSE App Store' }.freeze
[runner, nse].each do |target|
  CONFIGS.each do |name|
    config = target.build_configuration_list[name]
    set.(config, 'DEVELOPMENT_TEAM', TEAM)
    if name == 'Release'
      set.(config, 'CODE_SIGN_STYLE', 'Manual')
      set.(config, 'CODE_SIGN_IDENTITY', 'Apple Distribution')
      set.(config, 'CODE_SIGN_IDENTITY[sdk=iphoneos*]', 'Apple Distribution')
      set.(config, 'PROVISIONING_PROFILE_SPECIFIER', PROFILES.fetch(target.name))
    else
      set.(config, 'CODE_SIGN_STYLE', 'Automatic')
    end
  end
end

if changed || project.dirty?
  project.save
  puts "updated #{project_path}"
else
  puts "#{project_path} already up to date"
end

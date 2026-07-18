cask "hive" do
  arch arm: "aarch64", intel: "x64"

  version "0.1.0"
  # Fill these from the released DMGs before publishing:
  #   shasum -a 256 Hive_#{version}_aarch64.dmg   # → arm
  #   shasum -a 256 Hive_#{version}_x64.dmg        # → intel
  sha256 arm:   "0000000000000000000000000000000000000000000000000000000000000000",
         intel: "0000000000000000000000000000000000000000000000000000000000000000"

  url "https://github.com/honeyhive-ai/hive/releases/download/v#{version}/Hive_#{version}_#{arch}.dmg",
      verified: "github.com/honeyhive-ai/hive/"
  name "Hive"
  desc "Shared LLM workspace for developers — bring your own runtime"
  homepage "https://github.com/honeyhive-ai/hive"

  livecheck do
    url :url
    strategy :github_latest
  end

  app "Hive.app"

  # Remove user data on `brew uninstall --zap hive`.
  zap trash: [
    "~/Library/Application Support/com.hive.desktop",
    "~/Library/Caches/com.hive.desktop",
    "~/Library/Preferences/com.hive.desktop.plist",
    "~/Library/Saved Application State/com.hive.desktop.savedState",
  ]
end

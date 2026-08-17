import Foundation

#if os(macOS)
import AppKit
import Darwin
#else
import Glibc
#endif

enum InstallError: LocalizedError {
	case applicationNotInstalled
	case missingServiceExecutable
	case launchctlFailed(String)

	var errorDescription: String? {
		switch self {
		case .applicationNotInstalled:
			"Drag KeepPeek.app to Applications, then open it from there."
		case .missingServiceExecutable:
			"KeepPeek.app is incomplete. Download a new copy from the release page."
		case let .launchctlFailed(command):
			"Unable to \(command) the KeepPeek service."
		}
	}
}

let label = "com.keeppeek"
let environment = ProcessInfo.processInfo.environment
let fileManager = FileManager.default
let appURL = Bundle.main.bundleURL.resolvingSymlinksInPath()
let applicationsURL = URL(fileURLWithPath: "/Applications", isDirectory: true).resolvingSymlinksInPath()

func runLaunchctl(_ arguments: [String], allowFailure: Bool = false) throws {
	let process = Process()
	process.executableURL = URL(fileURLWithPath: "/bin/launchctl")
	process.arguments = arguments
	try process.run()
	process.waitUntilExit()

	if !allowFailure && process.terminationStatus != 0 {
		throw InstallError.launchctlFailed(arguments.first ?? "run")
	}
}

func installService() throws {
	guard appURL.deletingLastPathComponent() == applicationsURL else {
		throw InstallError.applicationNotInstalled
	}

	let serviceExecutable = appURL.appending(path: "Contents/Resources/keeppeek")
	guard fileManager.isExecutableFile(atPath: serviceExecutable.path) else {
		throw InstallError.missingServiceExecutable
	}

	let homeURL = fileManager.homeDirectoryForCurrentUser
	let libraryURL = homeURL.appending(path: "Library", directoryHint: .isDirectory)
	let launchAgentsURL = libraryURL.appending(path: "LaunchAgents", directoryHint: .isDirectory)
	let logsURL = libraryURL.appending(path: "Logs", directoryHint: .isDirectory)
	try fileManager.createDirectory(at: launchAgentsURL, withIntermediateDirectories: true)
	try fileManager.createDirectory(at: logsURL, withIntermediateDirectories: true)

	let plistURL = launchAgentsURL.appending(path: "\(label).plist")
	let plist: [String: Any] = [
		"Label": label,
		"ProgramArguments": [serviceExecutable.path],
		"EnvironmentVariables": ["HOME": homeURL.path],
		"KeepAlive": ["SuccessfulExit": false],
		"ProcessType": "Background",
		"RunAtLoad": true,
		"StandardErrorPath": logsURL.appending(path: "keeppeek-error.log").path,
		"StandardOutPath": logsURL.appending(path: "keeppeek.log").path,
		"ThrottleInterval": 10,
	]
	let plistData = try PropertyListSerialization.data(fromPropertyList: plist, format: .xml, options: 0)
	try plistData.write(to: plistURL, options: .atomic)

	let domain = "gui/\(getuid())"
	try runLaunchctl(["bootout", "\(domain)/\(label)"], allowFailure: true)
	try runLaunchctl(["bootstrap", domain, plistURL.path])
	try runLaunchctl(["enable", "\(domain)/\(label)"])
	try runLaunchctl(["kickstart", "-k", "\(domain)/\(label)"])
}

func showResult(title: String, message: String) {
	guard environment["KEEPPEEK_NO_UI"] != "1" else {
		print(message)
		return
	}

	#if os(macOS)
	let application = NSApplication.shared
	application.setActivationPolicy(.regular)
	application.activate(ignoringOtherApps: true)
	let alert = NSAlert()
	alert.messageText = title
	alert.informativeText = message
	alert.runModal()
	#else
	print("\(title): \(message)")
	#endif
}

do {
	try installService()
	showResult(
		title: "KeepPeek is running",
		message: "KeepPeek will start automatically when you log in. Logs are available in ~/Library/Logs/KeepPeek."
	)
} catch {
	showResult(title: "KeepPeek could not start", message: error.localizedDescription)
	exit(1)
}

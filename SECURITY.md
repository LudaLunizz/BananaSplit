# Security policy

## Reporting a vulnerability

Please report it **privately**, not in a public issue.

Go to the **Security** tab of this repository, choose **Report a vulnerability**, and fill in the form. Only you and the maintainer can see the report.

Please include:

- what happens and why you think it is a security problem,
- the steps to reproduce it (a command, an API request or a small config file helps),
- the version (`bananasplit --version`), the system (Linux distribution and package type, Windows or macOS version) and the audio server.

You will get an acknowledgement as soon as I see the report. For a verified problem I aim to have a fix released within about 90 days, and I will tell you if it will take longer. If you want to be credited in the release notes, say so.

## Supported versions

Only the latest release gets security fixes. Please check that the problem is still there in the latest release before you report it.

## What is in scope

- **The updater and the release signature**: anything that makes BananaSplit install a file that is not a release signed with BananaSplit's release key, or a release whose signed list names another version (`SHA256SUMS.txt` and `SHA256SUMS.txt.sig`, the check in `core/src/release.rs`).
- **The local API on `127.0.0.1:8790`** and the window page it serves: reaching it from another computer or from a web page in a browser, or doing what needs the token (quit, record, update, VBAN out) without it. The token is described in [docs/api.md](docs/api.md#the-token).
- **Files BananaSplit reads**: a config file, scene, MIDI binding or audio file that makes it crash, write outside its own folders, or run something.
- **VBAN input**: a packet that makes BananaSplit crash or read or write memory it should not.
- The release workflow and the files attached to a release.

The Windows cable driver is a separate project with its own policy: [Splitter](https://github.com/LudaLunizz/Splitter/security).

## What is not a vulnerability by itself

- **VBAN has no encryption or authentication** (that is the protocol, the same in Voicemeeter): anyone on your network can send audio to an input you opened or hear a stream you send. Use it on networks you trust.
- **Programs running as your own user** can read the token file and use the API; the token keeps out other users and web pages, not your own programs.
- The macOS app is signed ad hoc and not notarized; the README says so.

## Disclosure

Please give me the time above before you publish details. When a fix is released I will publish a GitHub security advisory that names the affected versions.

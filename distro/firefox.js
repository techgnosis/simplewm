// Firefox defaults for the simplewm distro, installed by build.sh as
// /usr/lib/firefox/defaults/pref/simplewm.js. Users can still change these in
// about:config.

// simplewm draws every window's title bar. Don't put the tabs in it, which also hides
// Firefox's own minimize/maximize/close buttons. Alt still shows the menu bar.
pref("browser.tabs.inTitlebar", 0);

// File dialogs from the desktop portal, which on this system is simplewm's File Picker.
pref("widget.use-xdg-desktop-portal.file-picker", 1);

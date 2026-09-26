// Chrome's font for a page's `sans-serif`, which is a preference of its own
// and not fontconfig's alias: Arial, on Linux. Inter is the one Ferrix
// prefers, as its fontconfig's sans-serif is (fonts/fonts.conf). The profile
// is new every boot, so the preference is set whenever the worker starts.
const setFonts = () => {
  chrome.fontSettings.setFont({ genericFamily: "sansserif", fontId: "Inter Variable" });
};
chrome.runtime.onInstalled.addListener(setFonts);
chrome.runtime.onStartup.addListener(setFonts);
setFonts();

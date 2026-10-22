export const createYoutubeCookieSync = ({ els, api }) => {
    let connectedBrowser = null;
    let busy = false;
    let loaded = false;

    const status = (message, error = false) => {
        els.youtubeCookieStatus.textContent = message;
        els.youtubeCookieStatus.classList.toggle('oj-status-error', error);
    };
    const render = () => {
        els.youtubeCookieBrowser.disabled = busy;
        els.syncYoutubeCookiesBtn.disabled = busy || !api.available || !loaded;
        els.disconnectYoutubeCookiesBtn.disabled = busy || !connectedBrowser;
    };
    const showConnection = () => {
        if (connectedBrowser) {
            els.youtubeCookieBrowser.value = connectedBrowser;
            status(
                `Cookie sync enabled for ${connectedBrowser}. Cookies will be read when the next YouTube request starts.`
            );
        } else {
            status('Cookie sync is off.');
        }
        render();
    };
    const load = async () => {
        try {
            const config = await api.getConfig();
            connectedBrowser = config.youtube_cookie_browser || null;
            loaded = true;
            showConnection();
        } catch {
            status('Cookie sync settings could not be loaded. Reopen Browser Import to retry.', true);
        }
    };
    const save = async browser => {
        if (busy || !api.available || !loaded) return;
        busy = true;
        render();
        try {
            const config = await api.patchConfig({ changes: { youtube_cookie_browser: browser } });
            connectedBrowser = config.youtube_cookie_browser || null;
            showConnection();
        } catch {
            status('Cookie sync settings could not be saved. Please try again.', true);
        } finally {
            busy = false;
            render();
        }
    };
    const bindEvents = () => {
        els.syncYoutubeCookiesBtn.addEventListener('click', () => void save(els.youtubeCookieBrowser.value));
        els.disconnectYoutubeCookiesBtn.addEventListener('click', () => void save(null));
    };
    return Object.freeze({ load, bindEvents, isLoaded: () => loaded });
};

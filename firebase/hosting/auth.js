import { initializeApp } from 'https://www.gstatic.com/firebasejs/12.16.0/firebase-app.js';
import {
  browserSessionPersistence,
  getAuth,
  GoogleAuthProvider,
  setPersistence,
  signInWithPopup,
  signOut
} from 'https://www.gstatic.com/firebasejs/12.16.0/firebase-auth.js';

const config = {
  apiKey: 'AIzaSyBniZaZKTefW67CxPt7YbwtaBVdc-9YiVE',
  authDomain: 'findexcodeintelligence.firebaseapp.com',
  projectId: 'findexcodeintelligence',
  appId: '1:162317753137:web:342800e8f14ed49451ee10',
  messagingSenderId: '162317753137'
};

const button = document.querySelector('#sign-in');
const status = document.querySelector('#status');
const parameters = new URLSearchParams(location.search);
const callback = parseCallback(parameters.get('callback'));
const state = parameters.get('state') ?? '';

if (!callback || !/^[A-Za-z0-9-]{32,64}$/.test(state)) {
  button.disabled = true;
  setStatus('Please initiate sign-in from the Findex desktop, CLI, or TUI app. Direct sign-ins from this URL are not supported.', true);
}

button.addEventListener('click', async () => {
  button.disabled = true;
  setStatus('Opening Google sign-in…');
  const auth = getAuth(initializeApp(config));
  try {
    await setPersistence(auth, browserSessionPersistence);
    const result = await signInWithPopup(auth, new GoogleAuthProvider());
    const credential = GoogleAuthProvider.credentialFromResult(result);
    if (!credential?.idToken) throw new Error('Google did not return an identity token.');
    setStatus('Completing sign-in in Findex…');
    const response = await fetch(callback.href, {
      method: 'POST',
      mode: 'cors',
      cache: 'no-store',
      referrerPolicy: 'no-referrer',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ state, id_token: credential.idToken })
    });
    if (!response.ok) throw new Error(`Findex rejected the callback (${response.status}).`);
    await signOut(auth);
    setStatus('Signed in. You can close this tab.');
    button.textContent = 'Signed in';
  } catch (error) {
    setStatus(error instanceof Error ? error.message : String(error), true);
    button.disabled = false;
  }
});

function parseCallback(value) {
  try {
    const url = new URL(value ?? '');
    const validPort = Number(url.port) >= 1024 && Number(url.port) <= 65535;
    return url.protocol === 'http:' && url.hostname === '127.0.0.1'
      && url.pathname === '/auth/callback' && validPort ? url : null;
  } catch {
    return null;
  }
}

function setStatus(message, error = false) {
  status.textContent = message;
  status.classList.toggle('error', error);
}

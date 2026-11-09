import {StrictMode} from 'react';
import {createRoot} from 'react-dom/client';
import {InspectorClient} from './api/client';
import {App} from './app/App';
import type {InspectorApi} from './app/App';
import './styles/app.css';
const root = document.getElementById('root');
if (!root) throw new Error('Inspector root missing');
function boot(api: InspectorApi, synthetic: boolean) {
  if (root) createRoot(root).render(<StrictMode><App api={api} synthetic={synthetic}/></StrictMode>);
}
if (import.meta.env.DEV) {
  void import('./fixtures/dev').then(module => boot(module.syntheticApi(), true));
} else {
  boot(new InspectorClient(new URLSearchParams(location.hash.slice(1)).get('token')), false);
}

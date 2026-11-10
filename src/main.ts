import { mount } from 'svelte';
import App from './App.svelte';
import './app.css';

const target = document.getElementById('app');
if (!target) throw new Error('缺少应用挂载节点 #app。');

const app = mount(App, { target });
export default app;

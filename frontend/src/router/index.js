import { createRouter, createWebHashHistory } from 'vue-router';
import ChatView from '../views/chat/ChatView.vue';
import LoginView from '../views/auth/LoginView.vue';

// Define application routes
const routes = [
  {
    path: '/',
    name: 'chat',
    component: ChatView,
    meta: { requiresAuth: true },
  },
  {
    path: '/login',
    name: 'login',
    component: LoginView,
  },
  {
    path: '/:pathMatch(.*)*',
    redirect: '/',
  },
];

// Create router instance
const router = createRouter({
  history: createWebHashHistory(),
  routes,
});

export default router;

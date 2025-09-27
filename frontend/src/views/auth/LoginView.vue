<template>
  <main class="auth" data-test="auth-view">
    <section class="panel" aria-labelledby="auth-heading">
      <header class="panel__header">
        <h1 id="auth-heading">Mesh-Talk</h1>
        <p class="tagline">Secure mesh messaging for your local network.</p>
      </header>

      <nav class="auth-switch" role="tablist" aria-label="Authentication mode">
        <button
          type="button"
          role="tab"
          class="auth-switch__option"
          data-test="mode-login"
          :aria-selected="isLoginMode"
          :tabindex="isLoginMode ? 0 : -1"
          @click="switchMode('login')"
        >
          Sign In
        </button>
        <button
          type="button"
          role="tab"
          class="auth-switch__option"
          data-test="mode-register"
          :aria-selected="!isLoginMode"
          :tabindex="isLoginMode ? -1 : 0"
          @click="switchMode('register')"
        >
          Create Account
        </button>
      </nav>

      <p v-if="infoCopy" class="panel__info">{{ infoCopy }}</p>

      <form
        v-if="isLoginMode"
        class="auth-form"
        data-test="login-form"
        autocomplete="on"
        novalidate
        @submit.prevent="handleLogin"
      >
        <div class="form-field" :class="{ 'has-error': loginErrors.username }">
          <label for="login-username">Username</label>
          <input
            id="login-username"
            v-model.trim="loginForm.username"
            name="username"
            type="text"
            autocomplete="username"
            inputmode="text"
            required
            data-test="login-username"
          />
          <p v-if="loginErrors.username" class="field-error">
            {{ loginErrors.username }}
          </p>
        </div>

        <div class="form-field" :class="{ 'has-error': loginErrors.password }">
          <label for="login-password">Password</label>
          <input
            id="login-password"
            v-model.trim="loginForm.password"
            name="password"
            type="password"
            autocomplete="current-password"
            required
            data-test="login-password"
          />
          <p v-if="loginErrors.password" class="field-error">
            {{ loginErrors.password }}
          </p>
        </div>

        <button
          type="submit"
          class="submit"
          data-test="login-submit"
          :disabled="isSubmitDisabled"
        >
          <span>{{ submitLabel }}</span>
        </button>
      </form>

      <form
        v-else
        class="auth-form"
        data-test="register-form"
        autocomplete="on"
        novalidate
        @submit.prevent="handleRegister"
      >
        <div
          class="form-field"
          :class="{ 'has-error': registerErrors.username }"
        >
          <label for="register-username">Display Name</label>
          <input
            id="register-username"
            v-model.trim="registerForm.username"
            name="username"
            type="text"
            autocomplete="username"
            required
            minlength="3"
            data-test="register-username"
          />
          <p v-if="registerErrors.username" class="field-error">
            {{ registerErrors.username }}
          </p>
        </div>

        <div class="form-grid">
          <div
            class="form-field"
            :class="{ 'has-error': registerErrors.password }"
          >
            <label for="register-password">Password</label>
            <input
              id="register-password"
              v-model="registerForm.password"
              name="password"
              type="password"
              autocomplete="new-password"
              required
              minlength="8"
              data-test="register-password"
            />
            <p v-if="registerErrors.password" class="field-error">
              {{ registerErrors.password }}
            </p>
          </div>

          <div
            class="form-field"
            :class="{ 'has-error': registerErrors.confirm }"
          >
            <label for="register-confirm">Confirm Password</label>
            <input
              id="register-confirm"
              v-model="registerForm.confirm"
              name="confirm"
              type="password"
              autocomplete="new-password"
              required
              minlength="8"
              data-test="register-confirm"
            />
            <p v-if="registerErrors.confirm" class="field-error">
              {{ registerErrors.confirm }}
            </p>
          </div>
        </div>

        <button
          type="submit"
          class="submit"
          data-test="register-submit"
          :disabled="isSubmitDisabled"
        >
          <span>{{ submitLabel }}</span>
        </button>
      </form>

      <p v-if="formError" class="form-error" data-test="form-error">
        {{ formError }}
      </p>

      <footer class="panel__footer">
        <button
          type="button"
          class="switch-link"
          data-test="mode-switch"
          @click="switchMode(isLoginMode ? 'register' : 'login')"
        >
          {{ switchCopy }}
        </button>
      </footer>
    </section>
  </main>
</template>

<script setup>
import { computed, reactive, ref, watch } from "vue";
import { useRouter, useRoute } from "vue-router";
import { useAuth } from "../../composables/auth/useAuth";

// Router and authentication
const router = useRouter();
const route = useRoute();
const { login, register, isAuthenticated, loading, error, setError } =
  useAuth();

// Form state
const mode = ref(route.query.mode === "register" ? "register" : "login");
const loginForm = reactive({ username: "", password: "" });
const registerForm = reactive({
  username: "",
  password: "",
  confirm: "",
});
const loginErrors = reactive({ username: "", password: "" });
const registerErrors = reactive({
  username: "",
  password: "",
  confirm: "",
});
const formError = ref("");

// Computed properties
const isLoginMode = computed(() => mode.value === "login");

const submitLabel = computed(() => {
  if (loading.value) {
    return isLoginMode.value ? "Signing in…" : "Creating account…";
  }
  return isLoginMode.value ? "Sign In" : "Create Account";
});

const infoCopy = computed(() =>
  isLoginMode.value
    ? "Use your Mesh-Talk node credentials to join the network."
    : "Create a node identity for this desktop client. Passwords must be at least 8 characters.",
);

const switchCopy = computed(() =>
  isLoginMode.value
    ? "Need an account? Create one instead"
    : "Already have an account? Sign in",
);

const isSubmitDisabled = computed(() => {
  if (loading.value) {
    return true;
  }

  if (isLoginMode.value) {
    return !loginForm.username.trim() || !loginForm.password.trim();
  }

  return (
    !registerForm.username.trim() ||
    registerForm.password.length < 8 ||
    registerForm.confirm.length < 8
  );
});

// Utility functions
function resetLoginErrors() {
  loginErrors.username = "";
  loginErrors.password = "";
}

function resetRegisterErrors() {
  registerErrors.username = "";
  registerErrors.password = "";
  registerErrors.confirm = "";
}

function handleValidationFailure(message) {
  formError.value = message;
  setError(message, { source: "auth.validation", toast: false });
}

function prepareRedirect() {
  const redirect = route.query.redirect;
  if (typeof redirect === "string" && redirect.length > 0) {
    return redirect;
  }
  return { name: "chat" };
}

// Form handling functions
async function handleLogin() {
  resetLoginErrors();
  formError.value = "";
  setError(null, { clearLastError: true, toast: false });

  if (!loginForm.username.trim()) {
    loginErrors.username = "Enter your username";
  }
  if (!loginForm.password.trim()) {
    loginErrors.password = "Enter your password";
  }

  if (loginErrors.username || loginErrors.password) {
    handleValidationFailure("Please fill in the highlighted fields.");
    return;
  }

  const result = await login(loginForm.username.trim(), loginForm.password);
  if (result?.success) {
    await router.push(prepareRedirect());
    return;
  }

  formError.value = error.value ?? "Unable to login";
}

async function handleRegister() {
  resetRegisterErrors();
  formError.value = "";
  setError(null, { clearLastError: true, toast: false });

  if (!registerForm.username.trim()) {
    registerErrors.username = "Choose a display name";
  } else if (registerForm.username.trim().length < 3) {
    registerErrors.username = "Display name must be at least 3 characters";
  }

  if (!registerForm.password) {
    registerErrors.password = "Create a password";
  } else if (registerForm.password.length < 8) {
    registerErrors.password = "Password must be at least 8 characters";
  }

  if (!registerForm.confirm) {
    registerErrors.confirm = "Confirm your password";
  } else if (registerForm.confirm !== registerForm.password) {
    registerErrors.confirm = "Passwords do not match";
  }

  if (
    registerErrors.username ||
    registerErrors.password ||
    registerErrors.confirm
  ) {
    handleValidationFailure("Fix the highlighted fields before continuing.");
    return;
  }

  const result = await register(
    registerForm.username.trim(),
    registerForm.password,
  );

  if (result?.success) {
    mode.value = "login";
    loginForm.username = registerForm.username.trim();
    loginForm.password = registerForm.password;
    registerForm.password = "";
    registerForm.confirm = "";
    return;
  }

  formError.value = error.value ?? "Registration failed";
}

// Mode switching functions
function switchMode(nextMode) {
  if (mode.value === nextMode) {
    return;
  }
  mode.value = nextMode;
  formError.value = "";
  setError(null, { clearLastError: true, toast: false });
  resetLoginErrors();
  resetRegisterErrors();
}

// Watchers
watch(
  () => route.query.mode,
  (value) => {
    if (value === "register") {
      mode.value = "register";
    } else {
      mode.value = "login";
    }
  },
);

watch(isAuthenticated, (signedIn) => {
  if (signedIn) {
    router.replace({ name: "chat" });
  }
});

watch(error, (value) => {
  if (!value) {
    formError.value = "";
  } else {
    formError.value = value;
  }
});

watch(mode, (nextMode) => {
  const query = { ...route.query };
  if (nextMode === "register") {
    query.mode = "register";
  } else {
    delete query.mode;
  }
  router.replace({ name: "login", query }).catch(() => {
    /* no-op */
  });
});
</script>

<style scoped>
.auth {
  min-height: 100vh;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 2rem;
  background: radial-gradient(
    circle at top,
    rgba(59, 130, 246, 0.18),
    transparent 55%
  );
}

.panel {
  width: min(520px, 100%);
  background: rgba(15, 23, 42, 0.85);
  backdrop-filter: blur(18px);
  border-radius: 28px;
  padding: clamp(2rem, 3vw, 2.6rem);
  border: 1px solid rgba(148, 163, 184, 0.18);
  display: flex;
  flex-direction: column;
  gap: 1.4rem;
  box-shadow: 0 25px 60px rgba(15, 23, 42, 0.45);
}

.panel__header {
  display: flex;
  flex-direction: column;
  gap: 0.4rem;
}

.panel__header h1 {
  margin: 0;
  font-size: clamp(2rem, 3vw, 2.6rem);
  letter-spacing: 0.08em;
  text-transform: uppercase;
}

.tagline {
  margin: 0;
  color: rgba(148, 163, 184, 0.85);
  font-size: 1rem;
}

.auth-switch {
  display: inline-flex;
  align-items: center;
  border-radius: 999px;
  padding: 0.25rem;
  background: rgba(30, 41, 59, 0.65);
  border: 1px solid rgba(148, 163, 184, 0.25);
  width: fit-content;
}

.auth-switch__option {
  min-width: 8.5rem;
  border: none;
  background: transparent;
  color: rgba(226, 232, 240, 0.78);
  padding: 0.45rem 1.2rem;
  border-radius: 999px;
  font-weight: 600;
  letter-spacing: 0.05em;
  transition:
    background 0.2s ease,
    color 0.2s ease;
  cursor: pointer;
}

.auth-switch__option[aria-selected="true"] {
  background: linear-gradient(
    135deg,
    rgba(59, 130, 246, 0.45),
    rgba(14, 165, 233, 0.6)
  );
  color: #f8fafc;
}

.panel__info {
  margin: 0;
  color: rgba(148, 163, 184, 0.85);
  font-size: 0.95rem;
  line-height: 1.5;
}

.auth-form {
  display: flex;
  flex-direction: column;
  gap: 1rem;
}

.form-field {
  display: flex;
  flex-direction: column;
  gap: 0.35rem;
}

.form-field label {
  font-size: 0.9rem;
  color: rgba(226, 232, 240, 0.92);
}

.form-field input {
  background: rgba(15, 23, 42, 0.75);
  border-radius: 14px;
  border: 1px solid rgba(148, 163, 184, 0.35);
  padding: 0.85rem 1rem;
  color: #f8fafc;
  font-size: 1rem;
  transition:
    border 0.2s ease,
    box-shadow 0.2s ease;
}

.form-field input:focus {
  outline: none;
  border-color: rgba(59, 130, 246, 0.65);
  box-shadow: 0 0 0 3px rgba(59, 130, 246, 0.25);
}

.form-field.has-error input {
  border-color: rgba(248, 113, 113, 0.65);
  box-shadow: 0 0 0 3px rgba(248, 113, 113, 0.2);
}

.field-error {
  margin: 0;
  font-size: 0.78rem;
  color: #fca5a5;
}

.form-grid {
  display: grid;
  gap: 1rem;
}

@media (min-width: 620px) {
  .form-grid {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}

.submit {
  border: none;
  border-radius: 14px;
  padding: 0.95rem 1.4rem;
  background: linear-gradient(
    135deg,
    rgba(59, 130, 246, 0.85),
    rgba(14, 165, 233, 0.9)
  );
  color: white;
  font-weight: 700;
  letter-spacing: 0.06em;
  cursor: pointer;
  transition:
    opacity 0.2s ease,
    transform 0.2s ease;
}

.submit:disabled {
  opacity: 0.6;
  cursor: not-allowed;
  transform: none;
}

.submit:not(:disabled):hover {
  transform: translateY(-1px);
}

.form-error {
  margin: 0;
  color: #fca5a5;
  background: rgba(127, 29, 29, 0.35);
  border: 1px solid rgba(127, 29, 29, 0.5);
  border-radius: 14px;
  padding: 0.75rem 1rem;
  font-size: 0.9rem;
}

.panel__footer {
  display: flex;
  justify-content: center;
}

.switch-link {
  border: none;
  background: transparent;
  color: rgba(148, 197, 255, 0.85);
  font-weight: 600;
  letter-spacing: 0.04em;
  cursor: pointer;
  text-decoration: underline;
}

.switch-link:hover {
  color: rgba(191, 219, 254, 0.95);
}

@media (max-width: 640px) {
  .panel {
    border-radius: 20px;
    padding: 1.8rem;
  }
}
</style>
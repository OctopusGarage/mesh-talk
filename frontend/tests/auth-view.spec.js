import { mount } from "@vue/test-utils";
import { createTestingPinia } from "@pinia/testing";
import { nextTick } from "vue";
import { useAppStore } from "../src/stores/appStore";
import LoginView from "../src/views/LoginView.vue";

describe("LoginView.vue", () => {
  let wrapper;
  let store;

  beforeEach(() => {
    wrapper = mount(LoginView, {
      global: {
        plugins: [
          createTestingPinia({
            createSpy: vi.fn,
            stubActions: false,
          }),
        ],
        mocks: {
          $route: {
            query: {},
          },
          $router: {
            replace: vi.fn(),
            push: vi.fn(),
          },
        },
      },
    });

    store = useAppStore();
  });

  afterEach(() => {
    wrapper.unmount();
    vi.clearAllMocks();
  });

  it("renders login form by default", () => {
    expect(wrapper.find('[data-test="login-form"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="register-form"]').exists()).toBe(false);
  });

  it("can switch to register mode", async () => {
    await wrapper.find('[data-test="mode-register"]').trigger("click");
    await nextTick();

    expect(wrapper.find('[data-test="login-form"]').exists()).toBe(false);
    expect(wrapper.find('[data-test="register-form"]').exists()).toBe(true);
  });

  it("validates login form", async () => {
    await wrapper.find('[data-test="login-form"]').trigger("submit.prevent");

    expect(wrapper.find('[data-test="form-error"]').text()).toContain(
      "Please fill in the highlighted fields",
    );

    await wrapper
      .find('[data-test="login-username"]')
      .setValue("alice");
    await wrapper.find('[data-test="login-form"]').trigger("submit.prevent");

    expect(wrapper.find('[data-test="form-error"]').text()).toContain(
      "Please fill in the highlighted fields",
    );
  });

  it("submits login form successfully", async () => {
    const loginMock = vi.fn().mockResolvedValue({ success: true });
    store.login = loginMock;

    await wrapper
      .find('[data-test="login-username"]')
      .setValue("alice");
    await wrapper
      .find('[data-test="login-password"]')
      .setValue("password123");
    await wrapper.find('[data-test="login-form"]').trigger("submit.prevent");

    expect(loginMock).toHaveBeenCalledWith("alice", "password123");
    wrapper.unmount();
  });

  it("handles login failure", async () => {
    const loginMock = vi.fn().mockResolvedValue({ success: false });
    store.login = loginMock;

    await wrapper
      .find('[data-test="login-username"]')
      .setValue("alice");
    await wrapper
      .find('[data-test="login-password"]')
      .setValue("password123");
    await wrapper.find('[data-test="login-form"]').trigger("submit.prevent");

    expect(wrapper.find('[data-test="form-error"]').text()).toBe(
      "Unable to login",
    );
    wrapper.unmount();
  });

  it("validates register form", async () => {
    await wrapper.find('[data-test="mode-register"]').trigger("click");
    await nextTick();

    await wrapper.find('[data-test="register-form"]').trigger("submit.prevent");

    expect(wrapper.find('[data-test="form-error"]').text()).toContain(
      "Fix the highlighted fields before continuing",
    );
  });

  it("submits register form successfully", async () => {
    await wrapper.find('[data-test="mode-register"]').trigger("click");
    await nextTick();

    const registerMock = vi.fn().mockResolvedValue({ success: true });
    store.register = registerMock;

    await wrapper
      .find('[data-test="register-username"]')
      .setValue("carol");
    await wrapper
      .find('[data-test="register-password"]')
      .setValue("password123");
    await wrapper
      .find('[data-test="register-confirm"]')
      .setValue("password123");
    await wrapper.find('[data-test="register-form"]').trigger("submit.prevent");

    expect(registerMock).toHaveBeenCalledWith("carol", "password123");
    await nextTick();
    expect(wrapper.find('[data-test="login-form"]').exists()).toBe(true);
    wrapper.unmount();
  });
});
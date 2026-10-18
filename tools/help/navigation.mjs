const navigation = document.querySelector("aside details");
const wide = window.matchMedia("(min-width: 721px)");
function adapt() {
  navigation.open = wide.matches;
}
adapt();
wide.addEventListener("change", adapt);

/* The new theme fades over the whole page as one, every transition held while it applies. */
export function setTheme(theme: 'light' | 'dark') {
  const apply = () => {
    const root = document.documentElement
    root.setAttribute('data-theme-swap', '')
    root.dataset.theme = theme
    // Reading the layout applies the new colours before the transitions return.
    void root.offsetHeight
    root.removeAttribute('data-theme-swap')
  }
  if ('startViewTransition' in document) document.startViewTransition(apply)
  else apply()
}

/* Until the visitor picks one, the device decides, and keeps deciding. */
export function followDevice() {
  matchMedia('(prefers-color-scheme: dark)').addEventListener('change', (event) => {
    if (!localStorage.getItem('theme')) setTheme(event.matches ? 'dark' : 'light')
  })
}

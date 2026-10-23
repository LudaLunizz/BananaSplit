/* A stand-in for libwebkit2gtk-4.1.so.0 with the five functions and three
 * signals BananaSplit's window uses (see src/shell.rs), and what WebKitGTK
 * watches for the page's prefers-color-scheme (GTK's prefer-dark setting). The "web view" is a
 * GtkLabel showing the URL it was asked to load, so the window process can
 * be tested with real GTK on a machine without WebKitGTK. Every call is
 * logged to $MOCKWEBKIT_LOG. Built by tests/window/run_window_test.sh. */
#include <glib-object.h>
#include <stdio.h>
#include <stdlib.h>

extern GType gtk_label_get_type(void);
extern void gtk_label_set_text(void *label, const char *text);
extern void *gtk_settings_get_default(void);

static GType view_type = 0;

static void logf_(const char *what, const char *arg) {
  const char *p = getenv("MOCKWEBKIT_LOG");
  if (!p) return;
  FILE *f = fopen(p, "a");
  if (f) { fprintf(f, "%s %s\n", what, arg ? arg : ""); fclose(f); }
}

static void class_init(gpointer klass, gpointer data) {
  (void)data;
  GType t = G_TYPE_FROM_CLASS(klass);
  g_signal_new("context-menu", t, G_SIGNAL_RUN_LAST, 0, NULL, NULL, NULL, G_TYPE_BOOLEAN, 3, G_TYPE_POINTER, G_TYPE_POINTER, G_TYPE_POINTER);
  g_signal_new("web-process-terminated", t, G_SIGNAL_RUN_LAST, 0, NULL, NULL, NULL, G_TYPE_NONE, 1, G_TYPE_INT);
  g_signal_new("load-failed", t, G_SIGNAL_RUN_LAST, 0, NULL, NULL, NULL, G_TYPE_BOOLEAN, 3, G_TYPE_INT, G_TYPE_STRING, G_TYPE_POINTER);
}

/* What WebKitGTK turns into prefers-color-scheme: GTK's "prefer dark". */
static void dark_changed(GObject *settings, GParamSpec *p, gpointer d) {
  (void)p; (void)d;
  gboolean dark = FALSE;
  g_object_get(settings, "gtk-application-prefer-dark-theme", &dark, NULL);
  logf_("prefer-dark", dark ? "1" : "0");
}

void *webkit_web_view_new(void) {
  static int watching = 0;
  if (!watching++) g_signal_connect(gtk_settings_get_default(), "notify::gtk-application-prefer-dark-theme", G_CALLBACK(dark_changed), NULL);
  if (!view_type) {
    GTypeQuery q;
    g_type_query(gtk_label_get_type(), &q);
    view_type = g_type_register_static_simple(gtk_label_get_type(), "MockWebKitWebView", q.class_size, class_init, q.instance_size, NULL, 0);
  }
  logf_("new", NULL);
  return g_object_new(view_type, NULL);
}

/* MOCKWEBKIT_FIRE=context-menu,load-failed,web-process-terminated: emit
 * those signals once, shortly after the first page load, and log what the
 * handlers returned. */
static gboolean fire(gpointer view) {
  const char *list = getenv("MOCKWEBKIT_FIRE");
  gchar **sigs = g_strsplit(list ? list : "", ",", 0);
  for (gchar **s = sigs; *s && **s; s++) {
    gboolean ret = FALSE;
    if (g_str_equal(*s, "context-menu")) g_signal_emit_by_name(view, "context-menu", NULL, NULL, NULL, &ret);
    else if (g_str_equal(*s, "web-process-terminated")) g_signal_emit_by_name(view, "web-process-terminated", 1);
    else if (g_str_equal(*s, "load-failed")) g_signal_emit_by_name(view, "load-failed", 0, "x", NULL, &ret);
    char b[96];
    snprintf(b, sizeof b, "%s -> %d", *s, ret);
    logf_("emit", b);
  }
  g_strfreev(sigs);
  return G_SOURCE_REMOVE;
}

void webkit_web_view_load_uri(void *v, const char *uri) {
  static int loads = 0;
  char b[600];
  snprintf(b, sizeof b, "MOCK WEBVIEW %s", uri);
  gtk_label_set_text(v, b);
  logf_("load_uri", uri);
  if (loads++ == 0 && getenv("MOCKWEBKIT_FIRE")) g_timeout_add(300, fire, v);
}

void webkit_web_view_reload(void *v) { (void)v; logf_("reload", NULL); }
void webkit_web_view_set_background_color(void *v, const void *rgba) { (void)v; (void)rgba; logf_("background", NULL); }
int webkit_hit_test_result_context_is_editable(void *h) { (void)h; return 0; }


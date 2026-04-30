use pyllow_extract::ast::{self, Expr, Stmt};
use pyllow_extract::{base_class_tail_name, callable_tail_name, ParsedModule};
use pyllow_types::{FileId, ImportKind, PluginResult};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::Path;

pub const PLUGIN_NAME: &str = "django";

/// Class names whose subclasses are framework-managed entry points.
const MODEL_BASES: &[&str] = &["Model", "AbstractUser", "AbstractBaseUser"];
const VIEW_BASES: &[&str] = &[
    "View",
    "APIView",
    "ViewSet",
    "GenericViewSet",
    "ModelViewSet",
    "ReadOnlyModelViewSet",
    "TemplateView",
    "ListView",
    "DetailView",
    "CreateView",
    "UpdateView",
    "DeleteView",
    "FormView",
    "RedirectView",
    "GenericAPIView",
    "ListAPIView",
    "RetrieveAPIView",
    "CreateAPIView",
    "DestroyAPIView",
    "UpdateAPIView",
];
const COMMAND_BASES: &[&str] = &["BaseCommand", "AppCommand", "LabelCommand"];
const ADMIN_BASES: &[&str] = &["ModelAdmin", "TabularInline", "StackedInline", "Admin"];

/// Decorators that mark a function as a Django signal receiver.
const SIGNAL_DECORATORS: &[&str] = &["receiver"];

pub fn discover(parsed: &FxHashMap<FileId, ParsedModule>) -> PluginResult {
    let entry_files: FxHashSet<FileId> = parsed
        .par_iter()
        .filter_map(|(id, module)| module_is_django_entry(module).then_some(*id))
        .collect();
    PluginResult {
        plugin_name: PLUGIN_NAME.to_string(),
        entry_files,
        ..Default::default()
    }
}

fn module_is_django_entry(module: &ParsedModule) -> bool {
    // Migrations directory is auto-discovered by Django regardless of imports.
    if path_is_migration(&module.path) {
        return true;
    }
    if !imports_django(module) {
        // Settings.py heuristic — settings modules don't always import django at top.
        if path_looks_like_settings(&module.path) && has_django_settings_keys(&module.suite) {
            return true;
        }
        return false;
    }
    if path_is_url_conf(&module.path) || has_urlpatterns(&module.suite) {
        return true;
    }
    module.suite.iter().any(stmt_marks_django_entry)
}

fn imports_django(module: &ParsedModule) -> bool {
    module.imports.iter().any(|i| {
        if !matches!(i.kind, ImportKind::Absolute) {
            return false;
        }
        i.raw == "django"
            || i.raw.starts_with("django.")
            || i.raw == "rest_framework"
            || i.raw.starts_with("rest_framework.")
    })
}

fn path_is_migration(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == "migrations")
        && path.extension().and_then(|s| s.to_str()) == Some("py")
}

fn path_is_url_conf(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n == "urls.py")
        .unwrap_or(false)
}

fn path_looks_like_settings(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n == "settings.py" || n.starts_with("settings_") || n == "asgi.py" || n == "wsgi.py")
        .unwrap_or(false)
}

fn has_urlpatterns(body: &[Stmt]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::Assign(a) => a.targets.iter().any(|t| {
            matches!(t, Expr::Name(n) if n.id.as_str() == "urlpatterns")
        }),
        Stmt::AnnAssign(a) => {
            matches!(a.target.as_ref(), Expr::Name(n) if n.id.as_str() == "urlpatterns")
        }
        _ => false,
    })
}

fn has_django_settings_keys(body: &[Stmt]) -> bool {
    const KEYS: &[&str] = &[
        "INSTALLED_APPS",
        "MIDDLEWARE",
        "DATABASES",
        "ROOT_URLCONF",
        "AUTH_USER_MODEL",
        "WSGI_APPLICATION",
    ];
    body.iter().any(|stmt| match stmt {
        Stmt::Assign(a) => a.targets.iter().any(|t| {
            matches!(t, Expr::Name(n) if KEYS.contains(&n.id.as_str()))
        }),
        Stmt::AnnAssign(a) => {
            matches!(a.target.as_ref(), Expr::Name(n) if KEYS.contains(&n.id.as_str()))
        }
        _ => false,
    })
}

fn stmt_marks_django_entry(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::ClassDef(c) => {
            if c.bases.iter().any(is_framework_base) {
                return true;
            }
            c.body.iter().any(stmt_marks_django_entry)
        }
        Stmt::FunctionDef(f) => {
            f.decorator_list.iter().any(is_signal_decorator)
                || f.body.iter().any(stmt_marks_django_entry)
        }
        Stmt::AsyncFunctionDef(f) => {
            f.decorator_list.iter().any(is_signal_decorator)
                || f.body.iter().any(stmt_marks_django_entry)
        }
        Stmt::If(s) => {
            s.body.iter().any(stmt_marks_django_entry)
                || s.orelse.iter().any(stmt_marks_django_entry)
        }
        Stmt::Try(s) => {
            s.body.iter().any(stmt_marks_django_entry)
                || s.handlers.iter().any(|h| {
                    let ast::ExceptHandler::ExceptHandler(eh) = h;
                    eh.body.iter().any(stmt_marks_django_entry)
                })
        }
        Stmt::With(s) => s.body.iter().any(stmt_marks_django_entry),
        Stmt::AsyncWith(s) => s.body.iter().any(stmt_marks_django_entry),
        _ => false,
    }
}

fn is_framework_base(expr: &Expr) -> bool {
    let Some(name) = base_class_tail_name(expr) else {
        return false;
    };
    MODEL_BASES.contains(&name)
        || VIEW_BASES.contains(&name)
        || COMMAND_BASES.contains(&name)
        || ADMIN_BASES.contains(&name)
}

fn is_signal_decorator(expr: &Expr) -> bool {
    callable_tail_name(expr)
        .map(|n| SIGNAL_DECORATORS.contains(&n))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyllow_extract::parse_source;
    use std::path::{Path, PathBuf};

    fn parse_at(path: &str, src: &str) -> ParsedModule {
        let mut m = parse_source(Path::new("test.py"), src).unwrap();
        m.path = PathBuf::from(path);
        m
    }

    fn parse(src: &str) -> ParsedModule {
        parse_source(Path::new("test.py"), src).unwrap()
    }

    #[test]
    fn detects_model_subclass() {
        let m = parse(
            "from django.db import models\nclass User(models.Model):\n    name = models.CharField(max_length=100)\n",
        );
        assert!(module_is_django_entry(&m));
    }

    #[test]
    fn detects_drf_viewset() {
        let m = parse(
            "from rest_framework import viewsets\nclass UserViewSet(viewsets.ModelViewSet):\n    queryset = []\n",
        );
        assert!(module_is_django_entry(&m));
    }

    #[test]
    fn detects_class_based_view() {
        let m = parse(
            "from django.views import View\nclass Home(View):\n    def get(self, request):\n        pass\n",
        );
        assert!(module_is_django_entry(&m));
    }

    #[test]
    fn detects_management_command() {
        let m = parse(
            "from django.core.management.base import BaseCommand\nclass Command(BaseCommand):\n    def handle(self, *args, **options):\n        pass\n",
        );
        assert!(module_is_django_entry(&m));
    }

    #[test]
    fn detects_admin_class() {
        let m = parse(
            "from django.contrib import admin\nfrom .models import User\nclass UserAdmin(admin.ModelAdmin):\n    list_display = (\"name\",)\n",
        );
        assert!(module_is_django_entry(&m));
    }

    #[test]
    fn detects_signal_receiver() {
        let m = parse(
            "from django.dispatch import receiver\nfrom django.db.models.signals import post_save\n@receiver(post_save)\ndef handler(sender, **kwargs):\n    pass\n",
        );
        assert!(module_is_django_entry(&m));
    }

    #[test]
    fn detects_urlpatterns() {
        let m = parse(
            "from django.urls import path\nurlpatterns = [path(\"\", lambda r: None)]\n",
        );
        assert!(module_is_django_entry(&m));
    }

    #[test]
    fn detects_migration_file_by_path() {
        let m = parse_at(
            "src/myapp/migrations/0001_initial.py",
            "from django.db import migrations\nclass Migration(migrations.Migration):\n    operations = []\n",
        );
        assert!(module_is_django_entry(&m));
    }

    #[test]
    fn detects_migration_without_django_import_via_path() {
        // Django generates migrations and they're picked up by the framework
        // regardless of imports — path-based detection is the safety net.
        let m = parse_at("apps/billing/migrations/0042_squash.py", "x = 1\n");
        assert!(module_is_django_entry(&m));
    }

    #[test]
    fn detects_settings_module() {
        let m = parse_at(
            "myproject/settings.py",
            "INSTALLED_APPS = [\"django.contrib.admin\"]\nDATABASES = {}\n",
        );
        assert!(module_is_django_entry(&m));
    }

    #[test]
    fn ignores_class_named_model_without_django_import() {
        let m = parse(
            "class Model:\n    pass\nclass Foo(Model):\n    pass\n",
        );
        assert!(!module_is_django_entry(&m));
    }

    #[test]
    fn ignores_unrelated_module() {
        let m = parse("import os\ndef f():\n    return 1\n");
        assert!(!module_is_django_entry(&m));
    }
}

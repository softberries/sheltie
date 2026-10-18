//! What the adapter reads from a `pom.xml`.

use std::collections::BTreeMap;

use crate::xml::{self, Element};

/// Maven plugins that run arbitrary code or add sources at build time.
pub(crate) const LEAKY_PLUGINS: &[&str] = &[
    "exec-maven-plugin",
    "maven-antrun-plugin",
    "build-helper-maven-plugin",
];

/// A reference to another artifact, as written (properties not yet interpolated).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Dep {
    pub(crate) group: String,
    pub(crate) artifact: String,
    /// Whether it asks for the artifact's test classes (`test-jar` type or `tests` classifier).
    pub(crate) tests: bool,
}

/// The `<parent>` element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Parent {
    pub(crate) group: Option<String>,
    pub(crate) artifact: String,
    pub(crate) version: Option<String>,
    /// `None` for the default (`../pom.xml`); empty when lookup in the repository is off.
    pub(crate) relative_path: Option<String>,
}

/// One POM.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Pom {
    pub(crate) group: Option<String>,
    pub(crate) artifact: Option<String>,
    pub(crate) version: Option<String>,
    pub(crate) parent: Option<Parent>,
    /// `<modules>` entries, including those of profiles.
    pub(crate) modules: Vec<String>,
    pub(crate) properties: BTreeMap<String, String>,
    /// Dependencies, build plugins, plugin dependencies and annotation processor paths,
    /// including those of profiles; never `<dependencyManagement>` or `<pluginManagement>`.
    pub(crate) deps: Vec<Dep>,
    /// Artifact ids of `<build><plugins>`, including those of profiles.
    pub(crate) plugins: Vec<String>,
}

impl Pom {
    /// Parses a POM.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let project = xml::parse(text)?;
        if project.name != "project" {
            return Err(format!("root element is <{}>, not <project>", project.name));
        }
        let owned = |s: &str| s.to_owned();
        let mut pom = Self {
            group: project.text_of("groupId").map(owned),
            artifact: project.text_of("artifactId").map(owned),
            version: project.text_of("version").map(owned),
            parent: project.child("parent").and_then(|p| {
                Some(Parent {
                    group: p.text_of("groupId").map(owned),
                    artifact: p.text_of("artifactId")?.to_owned(),
                    version: p.text_of("version").map(owned),
                    relative_path: p.child("relativePath").map(|r| r.text.trim().to_owned()),
                })
            }),
            ..Self::default()
        };
        if let Some(props) = project.child("properties") {
            for p in &props.children {
                pom.properties
                    .insert(p.name.clone(), p.text.trim().to_owned());
            }
        }
        let profiles: Vec<&Element> = project
            .child("profiles")
            .into_iter()
            .flat_map(|p| p.all("profile"))
            .collect();
        for scope in std::iter::once(&project).chain(profiles) {
            pom.read_scope(scope);
        }
        Ok(pom)
    }

    /// Reads the modules, dependencies and plugins of the project or of one profile.
    fn read_scope(&mut self, scope: &Element) {
        if let Some(modules) = scope.child("modules") {
            self.modules.extend(
                modules
                    .all("module")
                    .map(|m| m.text.trim().to_owned())
                    .filter(|m| !m.is_empty()),
            );
        }
        if let Some(deps) = scope.child("dependencies") {
            self.deps.extend(deps.all("dependency").filter_map(dep));
        }
        let plugins = scope
            .at(&["build", "plugins"])
            .into_iter()
            .flat_map(|p| p.all("plugin"));
        for plugin in plugins {
            let Some(artifact) = plugin.text_of("artifactId") else {
                continue;
            };
            self.plugins.push(artifact.to_owned());
            self.deps.push(Dep {
                group: plugin
                    .text_of("groupId")
                    .unwrap_or("org.apache.maven.plugins")
                    .to_owned(),
                artifact: artifact.to_owned(),
                tests: false,
            });
            if let Some(deps) = plugin.child("dependencies") {
                self.deps.extend(deps.all("dependency").filter_map(dep));
            }
            // Annotation processors built in the same repository run at compile time.
            let paths = plugin.at(&["configuration", "annotationProcessorPaths"]);
            self.deps.extend(
                paths
                    .into_iter()
                    .flat_map(|p| p.all("path"))
                    .filter_map(dep),
            );
        }
    }
}

fn dep(e: &Element) -> Option<Dep> {
    Some(Dep {
        group: e.text_of("groupId")?.to_owned(),
        artifact: e.text_of("artifactId")?.to_owned(),
        tests: e.text_of("type") == Some("test-jar") || e.text_of("classifier") == Some("tests"),
    })
}

/// Replaces `${name}` with properties, recursively (a few levels; unknown ones stay).
pub(crate) fn interpolate(text: &str, props: &BTreeMap<String, String>) -> String {
    let mut s = text.to_owned();
    for _ in 0..5 {
        if !s.contains("${") {
            break;
        }
        let mut out = String::with_capacity(s.len());
        let mut rest = s.as_str();
        while let Some(start) = rest.find("${") {
            out.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            match after.find('}') {
                Some(end) => {
                    let key = &after[..end];
                    match props.get(key) {
                        Some(v) => out.push_str(v),
                        None => out.push_str(&rest[start..start + 3 + end]),
                    }
                    rest = &after[end + 1..];
                }
                None => {
                    out.push_str(&rest[start..]);
                    rest = "";
                }
            }
        }
        out.push_str(rest);
        if out == s {
            break;
        }
        s = out;
    }
    s
}

/// A POM's text without comments, whitespace-normalised: what its dependents see.
pub(crate) fn normalized(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        out.push(' ');
        rest = match rest[start + 4..].find("-->") {
            Some(end) => &rest[start + 4 + end + 3..],
            None => "",
        };
    }
    out.push_str(rest);
    sheltie_adapter::scan::normalize(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PARENT: &str = r#"<project>
  <modelVersion>4.0.0</modelVersion>
  <groupId>com.acme</groupId>
  <artifactId>shop-parent</artifactId>
  <version>1.0.0</version>
  <packaging>pom</packaging>
  <properties><java.version>21</java.version><core.name>core</core.name></properties>
  <modules>
    <module>core</module>
    <!-- <module>old</module> -->
    <module>api</module>
  </modules>
  <profiles>
    <profile><id>extra</id><modules><module>tools</module></modules></profile>
  </profiles>
  <dependencyManagement>
    <dependencies>
      <dependency><groupId>com.acme</groupId><artifactId>managed</artifactId></dependency>
    </dependencies>
  </dependencyManagement>
  <build>
    <pluginManagement><plugins><plugin><artifactId>maven-antrun-plugin</artifactId></plugin></plugins></pluginManagement>
  </build>
</project>"#;

    const CHILD: &str = r#"<project>
  <parent>
    <groupId>com.acme</groupId>
    <artifactId>shop-parent</artifactId>
    <version>1.0.0</version>
  </parent>
  <artifactId>api</artifactId>
  <dependencies>
    <dependency><groupId>${project.groupId}</groupId><artifactId>${core.name}</artifactId></dependency>
    <dependency><groupId>com.acme</groupId><artifactId>core</artifactId><type>test-jar</type><scope>test</scope></dependency>
    <dependency><groupId>org.junit.jupiter</groupId><artifactId>junit-jupiter</artifactId></dependency>
  </dependencies>
  <build>
    <plugins>
      <plugin>
        <groupId>org.codehaus.mojo</groupId>
        <artifactId>exec-maven-plugin</artifactId>
        <dependencies><dependency><groupId>com.acme</groupId><artifactId>codegen</artifactId></dependency></dependencies>
      </plugin>
      <plugin>
        <artifactId>maven-compiler-plugin</artifactId>
        <configuration><annotationProcessorPaths><path><groupId>com.acme</groupId><artifactId>processor</artifactId></path></annotationProcessorPaths></configuration>
      </plugin>
    </plugins>
  </build>
</project>"#;

    #[test]
    fn modules_and_management_sections() {
        let p = Pom::parse(PARENT).unwrap();
        assert_eq!(p.group.as_deref(), Some("com.acme"));
        assert_eq!(p.artifact.as_deref(), Some("shop-parent"));
        assert_eq!(p.modules, ["core", "api", "tools"]);
        assert!(p.deps.is_empty(), "{:?}", p.deps);
        assert!(p.plugins.is_empty());
        assert_eq!(p.properties["java.version"], "21");
        assert!(p.parent.is_none());
    }

    #[test]
    fn parent_dependencies_and_plugins() {
        let c = Pom::parse(CHILD).unwrap();
        assert_eq!(c.group, None);
        let parent = c.parent.unwrap();
        assert_eq!(parent.group.as_deref(), Some("com.acme"));
        assert_eq!(parent.artifact, "shop-parent");
        assert_eq!(parent.relative_path, None);
        let deps: Vec<(&str, &str, bool)> = c
            .deps
            .iter()
            .map(|d| (d.group.as_str(), d.artifact.as_str(), d.tests))
            .collect();
        assert_eq!(
            deps,
            [
                ("${project.groupId}", "${core.name}", false),
                ("com.acme", "core", true),
                ("org.junit.jupiter", "junit-jupiter", false),
                ("org.codehaus.mojo", "exec-maven-plugin", false),
                ("com.acme", "codegen", false),
                ("org.apache.maven.plugins", "maven-compiler-plugin", false),
                ("com.acme", "processor", false),
            ]
        );
        assert_eq!(c.plugins, ["exec-maven-plugin", "maven-compiler-plugin"]);
    }

    #[test]
    fn interpolation_and_normal_form() {
        let props: BTreeMap<String, String> = [
            ("a".to_owned(), "${b}-x".to_owned()),
            ("b".to_owned(), "B".to_owned()),
        ]
        .into();
        assert_eq!(interpolate("${a}/${b}/${c}", &props), "B-x/B/${c}");
        assert_eq!(interpolate("${unterminated", &props), "${unterminated");
        assert_eq!(
            normalized("<a>\n  <!-- note -->\n  <b>1</b>\n</a>"),
            normalized("<a> <b>1</b> </a>")
        );
        assert!(Pom::parse("<settings/>").is_err());
    }
}

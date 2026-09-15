//! Frame geometry uses CDP content quads, whose coordinates are relative to the
//! target root (not necessarily the immediate parent document).
use super::{Browser, surface::eval_value};
use crate::{Error, Result, engine::string};
use serde_json::{Value, json};

// A planar CSS transform is a homography after perspective division. Retain
// affine interpolation for parallelograms; four nonparallel edges require all
// nine matrix entries. Coordinates and matrices never reach input as NaN/Inf.
#[derive(Clone, Copy, Debug)]
struct Transform([[f64; 3]; 3]);
impl Transform {
    const ID: Self = Self([[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]]);
    fn quad(quad: &Value, width: f64, height: f64) -> Result<Self> {
        let values = quad
            .as_array()
            .filter(|v| v.len() == 8)
            .ok_or_else(|| Error::action("Frame content quad unavailable"))?;
        let q: Vec<f64> = values
            .iter()
            .map(|v| {
                v.as_f64()
                    .filter(|n| n.is_finite())
                    .ok_or_else(|| Error::action("Invalid frame quad"))
            })
            .collect::<Result<_>>()?;
        if !width.is_finite() || !height.is_finite() || width <= 0. || height <= 0. {
            return Err(Error::action("Frame has no area"));
        }
        // Folded, concave and edge-on projections cannot represent a visible
        // rectangular frame. Reflections remain valid (either winding works).
        let mut winding = 0_f64;
        for i in 0..4 {
            let j = (i + 1) % 4;
            let k = (i + 2) % 4;
            let (ax, ay) = (q[2 * j] - q[2 * i], q[2 * j + 1] - q[2 * i + 1]);
            let (bx, by) = (q[2 * k] - q[2 * j], q[2 * k + 1] - q[2 * j + 1]);
            let cross = ax * by - ay * bx;
            let tolerance = 1e-12 * (ax.hypot(ay) * bx.hypot(by)).max(1.);
            if !cross.is_finite() || cross.abs() <= tolerance || winding * cross < 0. {
                return Err(Error::action(
                    "Frame quad is degenerate or crosses a projection horizon",
                ));
            }
            winding = cross.signum();
        }
        let (dx1, dx2, dx3) = (q[2] - q[4], q[6] - q[4], q[0] - q[2] + q[4] - q[6]);
        let (dy1, dy2, dy3) = (q[3] - q[5], q[7] - q[5], q[1] - q[3] + q[5] - q[7]);
        let (g, h) = if dx3.abs() <= 1e-9 && dy3.abs() <= 1e-9 {
            (0., 0.)
        } else {
            let det = dx1 * dy2 - dx2 * dy1;
            ((dx3 * dy2 - dx2 * dy3) / det, (dx1 * dy3 - dx3 * dy1) / det)
        };
        let result = Self([
            [
                (q[2] - q[0] + g * q[2]) / width,
                (q[6] - q[0] + h * q[6]) / height,
                q[0],
            ],
            [
                (q[3] - q[1] + g * q[3]) / width,
                (q[7] - q[1] + h * q[7]) / height,
                q[1],
            ],
            [g / width, h / height, 1.],
        ])
        .finite()?;
        // Denominators are linear, so positivity at every rectangle corner
        // excludes a vanishing line anywhere within its visible content.
        for (x, y) in [(0., 0.), (width, 0.), (width, height), (0., height)] {
            result.point(x, y)?;
        }
        result.inverse()?;
        Ok(result)
    }
    fn finite(self) -> Result<Self> {
        if self.0.iter().flatten().all(|n| n.is_finite()) {
            Ok(self)
        } else {
            Err(Error::action("Invalid frame transform"))
        }
    }
    fn point(self, x: f64, y: f64) -> Result<(f64, f64)> {
        let m = self.0;
        let w = m[2][0] * x + m[2][1] * y + m[2][2];
        let scale = (m[2][0] * x).abs() + (m[2][1] * y).abs() + m[2][2].abs();
        if !x.is_finite() || !y.is_finite() || !w.is_finite() || w <= scale * 1e-12 {
            return Err(Error::action(
                "Point is on or behind the frame projection horizon",
            ));
        }
        let point = (
            (m[0][0] * x + m[0][1] * y + m[0][2]) / w,
            (m[1][0] * x + m[1][1] * y + m[1][2]) / w,
        );
        if !point.0.is_finite() || !point.1.is_finite() {
            return Err(Error::action("Invalid transformed frame point"));
        }
        Ok(point)
    }
    fn inverse(self) -> Result<Self> {
        let [[a, b, c], [d, e, f], [g, h, i]] = self.0;
        let adj = [
            [e * i - f * h, c * h - b * i, b * f - c * e],
            [f * g - d * i, a * i - c * g, c * d - a * f],
            [d * h - e * g, b * g - a * h, a * e - b * d],
        ];
        let terms = [a * adj[0][0], b * adj[1][0], c * adj[2][0]];
        let det = terms.iter().sum::<f64>();
        let scale = terms.iter().map(|n| n.abs()).sum::<f64>();
        if !det.is_finite() || det.abs() <= scale * 1e-12 {
            return Err(Error::action("Frame transform has no area"));
        }
        Self(adj.map(|row| row.map(|n| n / det))).finite()
    }
    fn after(self, other: Self) -> Result<Self> {
        let mut result = [[0.; 3]; 3];
        for (i, row) in result.iter_mut().enumerate() {
            for (j, entry) in row.iter_mut().enumerate() {
                *entry = (0..3).map(|k| self.0[i][k] * other.0[k][j]).sum();
            }
        }
        Self(result).finite()
    }
    fn bounds(self, b: &Value) -> Result<Value> {
        let number = |key: &str| {
            b[key]
                .as_f64()
                .filter(|n| n.is_finite())
                .ok_or_else(|| Error::action("Element bounds unavailable"))
        };
        let (x, y, w, h) = (
            number("x")?,
            number("y")?,
            number("width")?,
            number("height")?,
        );
        if w < 0. || h < 0. {
            return Err(Error::action("Invalid element bounds"));
        }
        let corners = [
            self.point(x, y)?,
            self.point(x + w, y)?,
            self.point(x + w, y + h)?,
            self.point(x, y + h)?,
        ];
        let left = corners.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
        let top = corners.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
        let right = corners
            .iter()
            .map(|p| p.0)
            .fold(f64::NEG_INFINITY, f64::max);
        let bottom = corners
            .iter()
            .map(|p| p.1)
            .fold(f64::NEG_INFINITY, f64::max);
        if !(right - left).is_finite() || !(bottom - top).is_finite() {
            return Err(Error::action("Invalid transformed element bounds"));
        }
        Ok(json!({"x":left,"y":top,"width":right-left,"height":bottom-top}))
    }
}

impl Browser {
    fn frame_transform(
        &mut self,
        tab: &str,
        frame: &Value,
        scroll: Option<&str>,
        hit_point: Option<(f64, f64)>,
    ) -> Result<Transform> {
        let mut frames = vec![frame.clone()];
        while let Some(parent) = frames.last().unwrap()["parentId"].as_str() {
            if frames.len() >= 32 || frames.iter().any(|f| f["id"] == parent) {
                return Err(Error::action("Invalid or excessive frame ancestry"));
            }
            frames.push(self.frame(tab, &json!({"frame":parent}))?);
        }
        let mut owners = vec![];
        for pair in frames.windows(2) {
            let (session, context) = self.world(tab, &pair[1])?;
            let owner = self.call(
                "DOM.getFrameOwner",
                json!({"frameId":pair[0]["id"]}),
                Some(&session),
            )?;
            owners.push((session, context, owner["backendNodeId"].clone()));
        }
        // Scroll ancestors from outermost to innermost, then read current quads.
        if let Some(alignment) = scroll {
            for (session, context, backend) in owners.iter().rev() {
                let node = self.call(
                    "DOM.resolveNode",
                    json!({"backendNodeId":backend,"executionContextId":context}),
                    Some(session),
                )?;
                let object = string(&node["object"], "objectId")?.to_owned();
                let result=self.call("Runtime.callFunctionOn",json!({"objectId":object,"functionDeclaration":"async function(alignment){this.scrollIntoView({block:alignment,inline:alignment,behavior:'instant'});await new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)));}","arguments":[{"value":alignment}],"returnByValue":true,"awaitPromise":true}),Some(session));
                if !result.as_ref().is_err_and(|e| e.code == -32006) {
                    let _ = self.call_maintenance(
                        "Runtime.releaseObject",
                        json!({"objectId":object}),
                        Some(session),
                    );
                }
                eval_value(result?)?;
            }
        }
        let mut transform = Transform::ID;
        let mut index = 0;
        while index < owners.len() {
            let (session, _, backend) = &owners[index];
            let size = self.evaluate(
                tab,
                "({width:innerWidth,height:innerHeight})",
                &json!({"frame":frames[index]["id"]}),
            )?;
            let quads = self.call(
                "DOM.getBoxModel",
                json!({"backendNodeId":backend}),
                Some(session),
            )?;
            let next = Transform::quad(
                &quads["model"]["content"],
                size["width"].as_f64().unwrap_or(0.),
                size["height"].as_f64().unwrap_or(0.),
            )?;
            transform = next.after(transform)?;
            if let Some((x, y)) = hit_point {
                let (x, y) = transform.point(x, y)?;
                // DOM hit testing addresses document coordinates; Input and
                // box-model quads use viewport coordinates. Account for scroll.
                let viewport=eval_value(self.call("Runtime.evaluate",json!({"expression":"({x:scrollX,y:scrollY,width:innerWidth,height:innerHeight})","returnByValue":true}),Some(session))?)?;
                if x < 0.
                    || y < 0.
                    || x >= viewport["width"].as_f64().unwrap_or(0.)
                    || y >= viewport["height"].as_f64().unwrap_or(0.)
                {
                    return Err(Error::action("Frame is obscured or outside viewport"));
                }
                let hit=self.call("DOM.getNodeForLocation",json!({"x":(x+viewport["x"].as_f64().unwrap_or(0.)).round() as i64,"y":(y+viewport["y"].as_f64().unwrap_or(0.)).round() as i64,"includeUserAgentShadowDOM":true}),Some(session)).map_err(|error|if error.message.contains("No node found"){Error::action("Frame is obscured or outside viewport")}else{error})?;
                if hit["backendNodeId"] != *backend && hit["frameId"] != frames[index]["id"] {
                    return Err(Error::action("Frame is obscured"));
                }
            }
            index += 1;
            // Same-process nested owners' quads already contain every ancestor
            // transform in this target; applying them twice misplaces input.
            while index < owners.len() && owners[index].0 == *session {
                index += 1;
            }
        }
        Ok(transform)
    }
    pub(super) fn frame_point(&mut self, tab: &str, args: &Value, point: &Value) -> Result<Value> {
        if args.get("frame").is_none() {
            return Ok(point.clone());
        }
        let frame = self.frame(tab, args)?;
        let hit_point = if args["force"] == true || args["operation"] == "screenshot" {
            None
        } else {
            Some((
                point["x"]
                    .as_f64()
                    .ok_or_else(|| Error::action("Point x unavailable"))?,
                point["y"]
                    .as_f64()
                    .ok_or_else(|| Error::action("Point y unavailable"))?,
            ))
        };
        let transform = self.frame_transform(
            tab,
            &frame,
            Some(args["scrollAlignment"].as_str().unwrap_or("center")),
            hit_point,
        )?;
        if args["operation"] == "screenshot" {
            return transform.bounds(point);
        }
        let (x, y) = transform.point(
            point["x"]
                .as_f64()
                .ok_or_else(|| Error::action("Point x unavailable"))?,
            point["y"]
                .as_f64()
                .ok_or_else(|| Error::action("Point y unavailable"))?,
        )?;
        let mut value = point.clone();
        value["x"] = json!(x);
        value["y"] = json!(y);
        Ok(value)
    }
    // Resolve an actual frame owner object returned by a renderer inspection.
    // No CSS guessing, URL matching, or external-profile discovery is involved.
    fn frame_from_object(
        &mut self,
        tab: &str,
        parent: &Value,
        session: &str,
        object: &str,
    ) -> Result<Value> {
        let result = self.call(
            "DOM.describeNode",
            json!({"objectId":object,"depth":1}),
            Some(session),
        );
        if !result.as_ref().is_err_and(|e| e.code == -32006) {
            let _ = self.call_maintenance(
                "Runtime.releaseObject",
                json!({"objectId":object}),
                Some(session),
            );
        }
        let node = result?;
        let id = node["node"]["frameId"]
            .as_str()
            .or_else(|| node["node"]["contentDocument"]["frameId"].as_str())
            .ok_or_else(|| Error::action("Focused frame detached"))?;
        // Cache parent identity before attaching an OOPIF omitted from root tree.
        match self.frame(tab, &json!({"frame":id})) {
            Ok(frame) => Ok(frame),
            Err(error) if error.code != -32006 => self.oopif_frame(tab, id, parent["id"].as_str()),
            Err(error) => Err(error),
        }
    }
    pub(super) fn focused_frame(&mut self, tab: &str) -> Result<Value> {
        let mut frame = self.frame(tab, &json!({}))?;
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..32 {
            if !seen.insert(frame["id"].clone().to_string()) {
                return Err(Error::action("Focused frame cycle"));
            }
            let (session, context) = self.world(tab, &frame)?;
            let response=self.call("Runtime.evaluate",json!({"contextId":context,"expression":"(()=>{let e=document.activeElement;while(e?.shadowRoot?.activeElement)e=e.shadowRoot.activeElement;return e?.matches('iframe,frame')?e:null;})()","returnByValue":false}),Some(&session))?;
            if response.get("exceptionDetails").is_some() {
                return Err(Error::action("Could not inspect focused frame"));
            }
            let Some(object) = response["result"]["objectId"].as_str() else {
                return Ok(frame);
            };
            frame = self.frame_from_object(tab, &frame, &session, object)?;
        }
        Err(Error::action("Focused frame nesting exceeds 32"))
    }
    pub(super) fn point_infos(&mut self, tab: &str, args: &Value) -> Result<Value> {
        let top_x = args["x"]
            .as_f64()
            .ok_or_else(|| Error::invalid("x required"))?;
        let top_y = args["y"]
            .as_f64()
            .ok_or_else(|| Error::invalid("y required"))?;
        let mut frame = self.frame(tab, args)?;
        let mut result = vec![];
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..32 {
            if !seen.insert(frame["id"].to_string()) {
                return Err(Error::action("Point frame cycle"));
            }
            let transform = self.frame_transform(tab, &frame, None, None)?;
            let (x, y) = transform.inverse()?.point(top_x, top_y)?;
            let mut selected = args.clone();
            selected["frame"] = frame["id"].clone();
            selected["x"] = json!(x);
            selected["y"] = json!(y);
            let infos = self.dom(tab, "point_info", &selected)?;
            let mut layer = vec![];
            for mut info in infos.as_array().cloned().unwrap_or_default() {
                info["boundingBox"] = transform.bounds(&info["boundingBox"])?;
                layer.push(info);
            }
            layer.extend(result);
            result = layer;
            let (session, context) = self.world(tab, &frame)?;
            let response=self.call("Runtime.evaluate",json!({"contextId":context,"expression":format!("(()=>{{let e=document.elementFromPoint({x},{y});while(e?.shadowRoot){{const child=e.shadowRoot.elementFromPoint({x},{y});if(!child||child===e)break;e=child;}}return e?.matches('iframe,frame')?e:null;}})()"),"returnByValue":false}),Some(&session))?;
            if response.get("exceptionDetails").is_some() {
                return Err(Error::action("Could not inspect point frame"));
            }
            let Some(object) = response["result"]["objectId"].as_str() else {
                return Ok(json!(result));
            };
            frame = self.frame_from_object(tab, &frame, &session, object)?;
        }
        Err(Error::action("Point frame nesting exceeds 32"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn content_quad_rotation_inverse_and_nested_composition() {
        let t = Transform::quad(
            &json!([10., 20., 10., 120., -40., 120., -40., 20.]),
            100.,
            50.,
        )
        .unwrap();
        assert_eq!(t.point(25., 10.).unwrap(), (0., 45.));
        assert_eq!(t.inverse().unwrap().point(0., 45.).unwrap(), (25., 10.));
        assert_eq!(t.after(t).unwrap().point(25., 10.).unwrap(), (-35., 20.));
        assert_eq!(
            t.bounds(&json!({"x":0,"y":0,"width":100,"height":50}))
                .unwrap(),
            json!({"x":-40.,"y":20.,"width":50.,"height":100.})
        );
        let perspective =
            Transform::quad(&json!([0, 0, 100, 0, 80, 100, 0, 100]), 100., 100.).unwrap();
        assert_eq!(perspective.point(50., 50.).unwrap(), (400. / 9., 500. / 9.));
    }
    #[test]
    fn projective_quad_reconstruction_grid_inverse_and_composition() {
        let source = Transform([[1.2, -0.15, 25.], [0.2, 0.9, 40.], [0.001, -0.0003, 1.]]);
        let outer = Transform([[0.8, 0.2, 12.], [-0.1, 1.1, 70.], [-0.0002, 0.0005, 1.]]);
        let (width, height) = (640., 480.);
        let q: Vec<_> = [(0., 0.), (width, 0.), (width, height), (0., height)]
            .into_iter()
            .flat_map(|(x, y)| {
                let (x, y) = source.point(x, y).unwrap();
                [x, y]
            })
            .collect();
        let actual = Transform::quad(&json!(q), width, height).unwrap();
        let inverse = actual.inverse().unwrap();
        let composed = outer.after(actual).unwrap();
        let check = |a: (f64, f64), b: (f64, f64)| {
            assert!(
                (a.0 - b.0).abs() < 1e-8 && (a.1 - b.1).abs() < 1e-8,
                "{a:?} != {b:?}"
            );
        };
        for i in 0..=16 {
            for j in 0..=12 {
                let (x, y) = (i as f64 * 40., j as f64 * 40.);
                let expected = source.point(x, y).unwrap();
                check(actual.point(x, y).unwrap(), expected);
                check(inverse.point(expected.0, expected.1).unwrap(), (x, y));
                check(
                    composed.point(x, y).unwrap(),
                    outer.point(expected.0, expected.1).unwrap(),
                );
            }
        }
        let reflected =
            Transform::quad(&json!([100, 0, 0, 0, 0, 100, 100, 100]), 100., 100.).unwrap();
        assert_eq!(reflected.point(20., 30.).unwrap(), (80., 30.));
        assert_eq!(
            reflected.inverse().unwrap().point(80., 30.).unwrap(),
            (20., 30.)
        );
    }
    #[test]
    fn invalid_projection_never_returns_an_input_coordinate() {
        for quad in [
            json!([0, 0, 100, 0, 0, 100, 100, 100]),
            json!([0, 0, 100, 0, 40, 40, 0, 100]),
            json!([0, 0, 100, 0, 100, 0, 0, 0]),
            json!([0, 0, 100, 0, 100, 100, 0, null]),
        ] {
            assert!(Transform::quad(&quad, 100., 100.).is_err(), "{quad}");
        }
        let q = json!([0, 0, 100, 0, 100, 100, 0, 100]);
        for size in [0., -1., f64::NAN, f64::INFINITY] {
            assert!(Transform::quad(&q, size, 100.).is_err());
            assert!(Transform::quad(&q, 100., size).is_err());
        }
        let horizon = Transform([[1., 0., 0.], [0., 1., 0.], [-0.01, 0., 1.]]);
        for x in [100., 101., f64::NAN, f64::INFINITY] {
            assert!(horizon.point(x, 10.).is_err());
        }
        assert!(
            horizon
                .bounds(&json!({"x":0,"y":0,"width":200,"height":100}))
                .is_err()
        );
        assert!(
            Transform::ID
                .bounds(&json!({"x":0,"y":0,"width":-1,"height":100}))
                .is_err()
        );
        assert!(Transform([[0.; 3]; 3]).inverse().is_err());
        assert!(
            Transform([[f64::MAX; 3]; 3])
                .after(Transform([[f64::MAX; 3]; 3]))
                .is_err()
        );
    }
}

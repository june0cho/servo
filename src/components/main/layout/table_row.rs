/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! CSS table formatting contexts.

use layout::box_::Box;
use layout::context::LayoutContext;
use layout::display_list_builder::{DisplayListBuilder, ExtraDisplayListData};
use layout::flow::{BaseFlow, TableRowFlowClass, FlowClass, Flow, ImmutableFlowUtils};
use layout::flow;
use layout::model::{MaybeAuto, Specified, Auto};
use layout::float_context::{FloatContext, PlacementInfo, Invalid, FloatType};

use std::cell::RefCell;
use geom::{Point2D, Rect};
use gfx::display_list::DisplayList;
use servo_util::geometry::Au;
use servo_util::geometry;

/// Information specific to floated blocks.
pub struct FloatedTableInfo {
    containing_width: Au,

    /// Offset relative to where the parent tried to position this flow
    rel_pos: Point2D<Au>,

    /// Index into the box list for inline floats
    index: Option<uint>,

    /// Number of floated children
    floated_children: uint,

    /// Left or right?
    float_type: FloatType
}

impl FloatedTableInfo {
    pub fn new(float_type: FloatType) -> FloatedTableInfo {
        FloatedTableInfo {
            containing_width: Au(0),
            rel_pos: Point2D(Au(0), Au(0)),
            index: None,
            floated_children: 0,
            float_type: float_type
        }
    }
}

/// A table formatting context.
pub struct TableRowFlow {
    /// Data common to all flows.
    base: BaseFlow,

    /// The associated box.
    box_: Option<Box>,

    //TODO: is_fixed should be bit fields to conserve memory.
    /// Position property
    is_fixed: bool,

    /// Additional floating flow members.
    float: Option<~FloatedTableInfo>,

    /// Column widths.
    col_widths: ~[Au],

    /// Column min widths.
    col_min_widths: ~[Au],

    /// Column pref widths.
    col_pref_widths: ~[Au],
}

impl TableRowFlow {
    pub fn new(base: BaseFlow) -> TableRowFlow {
        TableRowFlow {
            base: base,
            box_: None,
            is_fixed: false,
            float: None,
            col_widths: ~[],
            col_min_widths: ~[],
            col_pref_widths: ~[],
        }
    }

    pub fn from_box(base: BaseFlow, box_: Box, is_fixed: bool) -> TableRowFlow {
        TableRowFlow {
            base: base,
            box_: Some(box_),
            is_fixed: is_fixed,
            float: None,
            col_widths: ~[],
            col_min_widths: ~[],
            col_pref_widths: ~[],
        }
    }

    pub fn float_from_box(base: BaseFlow, float_type: FloatType, box_: Box) -> TableRowFlow {
        TableRowFlow {
            base: base,
            box_: Some(box_),
            is_fixed: false,
            float: Some(~FloatedTableInfo::new(float_type)),
            col_widths: ~[],
            col_min_widths: ~[],
            col_pref_widths: ~[],
        }
    }

    pub fn new_float(base: BaseFlow, float_type: FloatType) -> TableRowFlow {
        TableRowFlow {
            base: base,
            box_: None,
            is_fixed: false,
            float: Some(~FloatedTableInfo::new(float_type)),
            col_widths: ~[],
            col_min_widths: ~[],
            col_pref_widths: ~[],
        }
    }

    pub fn is_float(&self) -> bool {
        self.float.is_some()
    }

    pub fn teardown(&mut self) {
        for box_ in self.box_.iter() {
            box_.teardown();
        }
        self.box_ = None;
        self.float = None;
        self.col_widths = ~[];
        self.col_min_widths = ~[];
        self.col_pref_widths = ~[];
    }

    // inline(always) because this is only ever called by in-order or non-in-order top-level
    // methods
    #[inline(always)]
    fn assign_height_table_base(&mut self, ctx: &mut LayoutContext, inorder: bool) {
        let mut cur_y = Au::new(0);
        let mut clearance = Au::new(0);
        let mut top_offset = Au::new(0);
        let mut bottom_offset = Au::new(0);
        let mut left_offset = Au::new(0);
        let mut float_ctx = Invalid;

        if inorder {
            // Floats for blocks work like this:
            // self.floats_in -> child[0].floats_in
            // visit child[0]
            // child[i-1].floats_out -> child[i].floats_in
            // visit child[i]
            // repeat until all children are visited.
            // last_child.floats_out -> self.floats_out (done at the end of this method)
            float_ctx = self.base.floats_in.translate(Point2D(-left_offset, -top_offset));
            for kid in self.base.child_iter() {
                flow::mut_base(*kid).floats_in = float_ctx.clone();
                kid.assign_height_inorder(ctx);
                float_ctx = flow::mut_base(*kid).floats_out.clone();
            }
        }

        let mut collapsible = Au::new(0);
        let mut collapsing = Au::new(0);
        let mut margin_top = Au::new(0);
        let mut margin_bottom = Au::new(0);
        let mut top_margin_collapsible = false;
        let mut bottom_margin_collapsible = false;
        let mut first_in_flow = true;

        let mut max_y = Au::new(0);
        for kid in self.base.child_iter() {
            let child_node = flow::mut_base(*kid);
            child_node.position.origin.y = cur_y;
            max_y = geometry::max(max_y, child_node.position.size.height);
        }

        // TODO: A box's own margins collapse if the 'min-height' property is zero, and it has neither
        // top or bottom borders nor top or bottom padding, and it has a 'height' of either 0 or 'auto',
        // and it does not contain a line box, and all of its in-flow children's margins (if any) collapse.


        let mut height = max_y;

        for box_ in self.box_.iter() {
            let style = box_.style();

            // At this point, `height` is the height of the containing block, so passing `height`
            // as the second argument here effectively makes percentages relative to the containing
            // block per CSS 2.1 § 10.5.
            height = match MaybeAuto::from_style(style.Box.height, height) {
                Auto => height,
                Specified(value) => value
            };
        }

        let mut noncontent_height = Au::new(0);
        let screen_height = ctx.screen_size.height;
        for box_ in self.box_.iter() {
            let mut position = box_.position.get();

            let (y, h) = box_.get_y_coord_and_new_height_if_fixed(screen_height,
                                                                 height, Au(0), self.is_fixed);
            position.origin.y = y;
            height = h;

            if self.is_fixed {
                for kid in self.base.child_iter() {
                    let child_node = flow::mut_base(*kid);
                    child_node.position.origin.y = position.origin.y + top_offset;
                }
            }

            position.size.height = if self.is_fixed {
                height
            } else {
                height + noncontent_height
            };

            box_.position.set(position);
        }

        self.base.position.size.height = if self.is_fixed {
            height
        } else {
            height + noncontent_height
        };

        cur_y = cur_y + height;
        for kid in self.base.child_iter() {
            for kid_box_ in kid.as_table_cell().box_.iter() {
                let mut position = kid_box_.position.get();
                position.size.height = height;
                kid_box_.position.set(position);
            }
            let child_node = flow::mut_base(*kid);
            child_node.position.size.height = max_y;
        }

        if inorder {
            let extra_height = height - (cur_y - top_offset) + bottom_offset;
            self.base.floats_out = float_ctx.translate(Point2D(left_offset, -extra_height));
        } else {
            self.base.floats_out = self.base.floats_in.clone();
        }
    }

    fn assign_height_float_inorder(&mut self) {
        // assign_height_float was already called by the traversal function
        // so this is well-defined

        let mut height = Au(0);
        let mut clearance = Au(0);
        let mut full_noncontent_width = Au(0);
        let mut margin_height = Au(0);

        for box_ in self.box_.iter() {
            height = box_.position.get().size.height;
            clearance = match box_.clear() {
                None => Au(0),
                Some(clear) => self.base.floats_in.clearance(clear),
            };

            let noncontent_width = box_.padding.get().left + box_.padding.get().right +
                box_.border.get().left + box_.border.get().right;

            full_noncontent_width = noncontent_width + box_.margin.get().left +
                box_.margin.get().right;
            margin_height = box_.margin.get().top + box_.margin.get().bottom;
        }

        let info = PlacementInfo {
            width: self.base.position.size.width + full_noncontent_width,
            height: height + margin_height,
            ceiling: clearance,
            max_width: self.float.get_ref().containing_width,
            f_type: self.float.get_ref().float_type,
        };

        // Place the float and return the FloatContext back to the parent flow.
        // After, grab the position and use that to set our position.
        self.base.floats_out = self.base.floats_in.add_float(&info);
        self.float.get_mut_ref().rel_pos = self.base.floats_out.last_float_pos();
    }

    fn assign_height_float(&mut self, ctx: &mut LayoutContext) {
        // Now that we've determined our height, propagate that out.
        let has_inorder_children = self.base.num_floats > 0;
        if has_inorder_children {
            let mut float_ctx = FloatContext::new(self.float.get_ref().floated_children);
            for kid in self.base.child_iter() {
                flow::mut_base(*kid).floats_in = float_ctx.clone();
                kid.assign_height_inorder(ctx);
                float_ctx = flow::mut_base(*kid).floats_out.clone();
            }
        }
        let mut cur_y = Au(0);
        let mut top_offset = Au(0);

        for box_ in self.box_.iter() {
            top_offset = box_.margin.get().top + box_.border.get().top + box_.padding.get().top;
            cur_y = cur_y + top_offset;
        }

        for kid in self.base.child_iter() {
            let child_base = flow::mut_base(*kid);
            child_base.position.origin.y = cur_y;
            cur_y = cur_y + child_base.position.size.height;
        }

        let mut height = cur_y - top_offset;

        let mut noncontent_height;
        let box_ = self.box_.as_ref().unwrap();
        let mut position = box_.position.get();

        // The associated box is the border box of this flow.
        position.origin.y = box_.margin.get().top;

        noncontent_height = box_.padding.get().top + box_.padding.get().bottom +
            box_.border.get().top + box_.border.get().bottom;

        //TODO(eatkinson): compute heights properly using the 'height' property.
        let height_prop = MaybeAuto::from_style(box_.style().Box.height,
                                                Au::new(0)).specified_or_zero();

        height = geometry::max(height, height_prop) + noncontent_height;
        debug!("assign_height_float -- height: {}", height);

        position.size.height = height;
        box_.position.set(position);
    }

    pub fn build_display_list_table<E:ExtraDisplayListData>(
                                    &mut self,
                                    builder: &DisplayListBuilder,
                                    dirty: &Rect<Au>,
                                    list: &RefCell<DisplayList<E>>)
                                    -> bool {
        if self.is_float() {
            return self.build_display_list_float(builder, dirty, list);
        }

        let abs_rect = Rect(self.base.abs_position, self.base.position.size);
        if !abs_rect.intersects(dirty) {
            return true;
        }

        debug!("build_display_list_table: adding display element");

        // add box that starts table context
        for box_ in self.box_.iter() {
            box_.build_display_list(builder, dirty, self.base.abs_position, (&*self) as &Flow, list)
        }
        // TODO: handle any out-of-flow elements
        let this_position = self.base.abs_position;

        for child in self.base.child_iter() {
            let child_base = flow::mut_base(*child);
            child_base.abs_position = this_position + child_base.position.origin;
        }

        false
    }

    pub fn build_display_list_float<E:ExtraDisplayListData>(
                                    &mut self,
                                    builder: &DisplayListBuilder,
                                    dirty: &Rect<Au>,
                                    list: &RefCell<DisplayList<E>>)
                                    -> bool {
        let abs_rect = Rect(self.base.abs_position, self.base.position.size);
        if !abs_rect.intersects(dirty) {
            return true
        }

        let offset = self.base.abs_position + self.float.get_ref().rel_pos;
        // add box that starts table context
        for box_ in self.box_.iter() {
            box_.build_display_list(builder, dirty, offset, (&*self) as &Flow, list)
        }


        // TODO: handle any out-of-flow elements

        // go deeper into the flow tree
        for child in self.base.child_iter() {
            let child_base = flow::mut_base(*child);
            child_base.abs_position = offset + child_base.position.origin;
        }

        false
    }
}

impl Flow for TableRowFlow {
    fn class(&self) -> FlowClass {
        TableRowFlowClass
    }

    fn as_table_row<'a>(&'a mut self) -> &'a mut TableRowFlow {
        self
    }

    /* Recursively (bottom-up) determine the context's preferred and
    minimum widths.  When called on this context, all child contexts
    have had their min/pref widths set. This function must decide
    min/pref widths based on child context widths and dimensions of
    any boxes it is responsible for flowing.  */

    fn bubble_widths(&mut self, _: &mut LayoutContext) {
        let mut min_width = Au::new(0);
        let mut pref_width = Au::new(0);
        let mut num_floats = 0;

        /* find max width from child block contexts */
        for kid in self.base.child_iter() {
            assert!(kid.is_table_cell());

            for child_box in kid.as_table_cell().box_.iter() {
                let child_specified_width = MaybeAuto::from_style(child_box.style().Box.width,
                                                                  Au::new(0)).specified_or_zero();
                self.col_widths.push(child_specified_width);
            }

            let child_base = flow::mut_base(*kid);
            self.col_min_widths.push(child_base.min_width);
            self.col_pref_widths.push(child_base.pref_width);
            min_width = min_width + child_base.min_width;
            pref_width = pref_width + child_base.pref_width;
            num_floats = num_floats + child_base.num_floats;
        }

        if self.is_float() {
            self.base.num_floats = 1;
            self.float.get_mut_ref().floated_children = num_floats;
        } else {
            self.base.num_floats = num_floats;
        }

        self.base.min_width = min_width;
        self.base.pref_width = geometry::max(min_width, pref_width);
        println!(" ROW{:?} {:?} {:?}", self.debug_str(), self.col_min_widths, self.col_pref_widths);
    }

    /// Recursively (top-down) determines the actual width of child contexts and boxes. When called
    /// on this context, the context has had its width set by the parent context.
    ///
    /// Dual boxes consume some width first, and the remainder is assigned to all child (block)
    /// contexts.
    fn assign_widths(&mut self, ctx: &mut LayoutContext) {
        debug!("assign_widths({}): assigning width for flow {}",
               if self.is_float() {
                   "float"
               } else {
                   "table_row"
               },
               self.base.id);

        // The position was set to the containing block by the flow's parent.
        let mut remaining_width = self.base.position.size.width;
        let mut x_offset = Au::new(0);

        if self.is_float() {
            self.float.get_mut_ref().containing_width = remaining_width;

            // Parent usually sets this, but floats are never inorder
            self.base.flags_info.flags.set_inorder(false);
        }

        for box_ in self.box_.iter() {
            let style = box_.style();

            // The text alignment of a table_row flow is the text alignment of its box's style.
            self.base.flags_info.flags.set_text_align(style.Text.text_align);

            let screen_size = ctx.screen_size;
            let (x, w) = box_.get_x_coord_and_new_width_if_fixed(screen_size.width,
                                                                 screen_size.height,
                                                                 remaining_width,
                                                                 box_.offset(),
                                                                 self.is_fixed);
            x_offset = x;
            remaining_width = w;

            // The associated box is the border box of this flow.
            let mut position_ref = box_.position.borrow_mut();
            if self.is_fixed {
                position_ref.get().origin.x = x_offset;
            }
            position_ref.get().size.width = remaining_width;
        }

        if self.is_float() {
            self.base.position.size.width = remaining_width;
        }

        let has_inorder_children = if self.is_float() {
            self.base.num_floats > 0
        } else {
            self.base.flags_info.flags.inorder() || self.base.num_floats > 0
        };

        // FIXME(ksh8281): avoid copy
        let flags_info = self.base.flags_info.clone();
        let mut i = 0;
        for kid in self.base.child_iter() {
            assert!(kid.is_table_cell());

            let child_base = flow::mut_base(*kid);
            child_base.position.origin.x = x_offset;
            child_base.position.size.width = self.col_widths[i];
            child_base.flags_info.flags.set_inorder(has_inorder_children);

            x_offset = x_offset + self.col_widths[i];
            i += 1;

            if !child_base.flags_info.flags.inorder() {
                child_base.floats_in = FloatContext::new(0);
            }

            // Per CSS 2.1 § 16.3.1, text decoration propagates to all children in flow.
            //
            // TODO(pcwalton): When we have out-of-flow children, don't unconditionally propagate.
            child_base.flags_info.propagate_text_decoration_from_parent(&flags_info);

            child_base.flags_info.propagate_text_alignment_from_parent(&flags_info)
        }
    }

    fn assign_height_inorder(&mut self, ctx: &mut LayoutContext) {
        if self.is_float() {
            debug!("assign_height_inorder_float: assigning height for float {}", self.base.id);
            self.assign_height_float_inorder();
        } else {
            debug!("assign_height_inorder: assigning height for table_row {}", self.base.id);
            self.assign_height_table_base(ctx, true);
        }
    }

    fn assign_height(&mut self, ctx: &mut LayoutContext) {
        if self.is_float() {
            debug!("assign_height_float: assigning height for float {}", self.base.id);
            self.assign_height_float(ctx);
        } else {
            debug!("assign_height: assigning height for table_row {}", self.base.id);
            self.assign_height_table_base(ctx, false);
        }
    }

    fn collapse_margins(&mut self,
                        top_margin_collapsible: bool,
                        first_in_flow: &mut bool,
                        margin_top: &mut Au,
                        top_offset: &mut Au,
                        collapsing: &mut Au,
                        collapsible: &mut Au) {
        if self.is_float() {
            // Margins between a floated box and any other box do not collapse.
            *collapsing = Au::new(0);
            return;
        }

        for box_ in self.box_.iter() {
            // The top margin collapses with its first in-flow block-level child's
            // top margin if the parent has no top border, no top padding.
            if *first_in_flow && top_margin_collapsible {
                // If top-margin of parent is less than top-margin of its first child,
                // the parent box goes down until its top is aligned with the child.
                if *margin_top < box_.margin.get().top {
                    // TODO: The position of child floats should be updated and this
                    // would influence clearance as well. See #725
                    let extra_margin = box_.margin.get().top - *margin_top;
                    *top_offset = *top_offset + extra_margin;
                    *margin_top = box_.margin.get().top;
                }
            }
            // The bottom margin of an in-flow block-level element collapses
            // with the top margin of its next in-flow block-level sibling.
            *collapsing = geometry::min(box_.margin.get().top, *collapsible);
            *collapsible = box_.margin.get().bottom;
        }

        *first_in_flow = false;
    }

    fn debug_str(&self) -> ~str {
        let txt = if self.is_float() {
            ~"FloatFlow: "
        } else {
            ~"TableRowFlow: "
        };
        txt.append(match self.box_ {
            Some(ref rb) => rb.debug_str(),
            None => ~"",
        })
    }
}


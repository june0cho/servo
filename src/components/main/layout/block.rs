/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! CSS block formatting contexts.

use layout::box_::Box;
use layout::context::LayoutContext;
use layout::display_list_builder::{DisplayListBuilder, ExtraDisplayListData};
use layout::flow::{BaseFlow, BlockFlowClass, FlowClass, Flow, ImmutableFlowUtils};
use layout::flow;
use layout::model::{MaybeAuto, Specified, Auto, specified_or_none, specified};
use layout::float_context::{FloatContext, PlacementInfo, Invalid, FloatType};

use std::cell::RefCell;
use style::ComputedValues;
use geom::{Point2D, Rect, SideOffsets2D};
use gfx::display_list::DisplayList;
use servo_util::geometry::Au;
use servo_util::geometry;

/// Information specific to floated blocks.
pub struct FloatedBlockInfo {
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

impl FloatedBlockInfo {
    pub fn new(float_type: FloatType) -> FloatedBlockInfo {
        FloatedBlockInfo {
            containing_width: Au(0),
            rel_pos: Point2D(Au(0), Au(0)),
            index: None,
            floated_children: 0,
            float_type: float_type
        }
    }
}

/// A block formatting context.
pub struct BlockFlow {
    /// Data common to all flows.
    base: BaseFlow,

    /// The associated box.
    box_: Option<Box>,

    //TODO: is_fixed and is_root should be bit fields to conserve memory.
    /// Whether this block flow is the root flow.
    is_root: bool,

    is_fixed: bool,

    /// Additional floating flow members.
    float: Option<~FloatedBlockInfo>
}

impl BlockFlow {
    pub fn new(base: BaseFlow) -> BlockFlow {
        BlockFlow {
            base: base,
            box_: None,
            is_root: false,
            is_fixed: false,
            float: None
        }
    }

    pub fn from_box(base: BaseFlow, box_: Box, is_fixed: bool) -> BlockFlow {
        BlockFlow {
            base: base,
            box_: Some(box_),
            is_root: false,
            is_fixed: is_fixed,
            float: None
        }
    }

    pub fn float_from_box(base: BaseFlow, float_type: FloatType, box_: Box) -> BlockFlow {
        BlockFlow {
            base: base,
            box_: Some(box_),
            is_root: false,
            is_fixed: false,
            float: Some(~FloatedBlockInfo::new(float_type))
        }
    }

    pub fn new_root(base: BaseFlow) -> BlockFlow {
        BlockFlow {
            base: base,
            box_: None,
            is_root: true,
            is_fixed: false,
            float: None
        }
    }

    pub fn new_float(base: BaseFlow, float_type: FloatType) -> BlockFlow {
        BlockFlow {
            base: base,
            box_: None,
            is_root: false,
            is_fixed: false,
            float: Some(~FloatedBlockInfo::new(float_type))
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
    }

    /// Computes left and right margins and width based on CSS 2.1 section 10.3.3.
    /// Requires borders and padding to already be computed.
    pub fn compute_horiz(&self,
                     width: MaybeAuto,
                     left_margin: MaybeAuto,
                     right_margin: MaybeAuto,
                     available_width: Au)
                     -> (Au, Au, Au) {
        // If width is not 'auto', and width + margins > available_width, all 'auto' margins are
        // treated as 0.
        let (left_margin, right_margin) = match width {
            Auto => (left_margin, right_margin),
            Specified(width) => {
                let left = left_margin.specified_or_zero();
                let right = right_margin.specified_or_zero();

                if((left + right + width) > available_width) {
                    (Specified(left), Specified(right))
                } else {
                    (left_margin, right_margin)
                }
            }
        };

        //Invariant: left_margin_Au + width_Au + right_margin_Au == available_width
        let (left_margin_Au, width_Au, right_margin_Au) = match (left_margin, width, right_margin) {
            //If all have a computed value other than 'auto', the system is over-constrained and we need to discard a margin.
            //if direction is ltr, ignore the specified right margin and solve for it. If it is rtl, ignore the specified
            //left margin. FIXME(eatkinson): this assumes the direction is ltr
            (Specified(margin_l), Specified(width), Specified(_margin_r)) => (margin_l, width, available_width - (margin_l + width )),

            //If exactly one value is 'auto', solve for it
            (Auto, Specified(width), Specified(margin_r)) => (available_width - (width + margin_r), width, margin_r),
            (Specified(margin_l), Auto, Specified(margin_r)) => (margin_l, available_width - (margin_l + margin_r), margin_r),
            (Specified(margin_l), Specified(width), Auto) => (margin_l, width, available_width - (margin_l + width)),

            //If width is set to 'auto', any other 'auto' value becomes '0', and width is solved for
            (Auto, Auto, Specified(margin_r)) => (Au::new(0), available_width - margin_r, margin_r),
            (Specified(margin_l), Auto, Auto) => (margin_l, available_width - margin_l, Au::new(0)),
            (Auto, Auto, Auto) => (Au::new(0), available_width, Au::new(0)),

            //If left and right margins are auto, they become equal
            (Auto, Specified(width), Auto) => {
                let margin = (available_width - width).scale_by(0.5);
                (margin, width, margin)
            }

        };
        //return values in same order as params
        (width_Au, left_margin_Au, right_margin_Au)
    }

    pub fn compute_block_margins(&self, box_: &Box, remaining_width: Au, available_width: Au)
                             -> (Au, Au, Au) {
        let style = box_.style();

        let (width, maybe_margin_left, maybe_margin_right) =
            (MaybeAuto::from_style(style.Box.width, remaining_width),
             MaybeAuto::from_style(style.Margin.margin_left, remaining_width),
             MaybeAuto::from_style(style.Margin.margin_right, remaining_width));

        let (width, margin_left, margin_right) = self.compute_horiz(width,
                                                                    maybe_margin_left,
                                                                    maybe_margin_right,
                                                                    available_width);

        // If the tentative used width is greater than 'max-width', width should be recalculated,
        // but this time using the computed value of 'max-width' as the computed value for 'width'.
        let (width, margin_left, margin_right) = {
            match specified_or_none(style.Box.max_width, remaining_width) {
                Some(value) if value < width => self.compute_horiz(Specified(value),
                                                                   maybe_margin_left,
                                                                   maybe_margin_right,
                                                                   available_width),
                _ => (width, margin_left, margin_right)
            }
        };

        // If the resulting width is smaller than 'min-width', width should be recalculated,
        // but this time using the value of 'min-width' as the computed value for 'width'.
        let (width, margin_left, margin_right) = {
            let computed_min_width = specified(style.Box.min_width, remaining_width);
            if computed_min_width > width {
                self.compute_horiz(Specified(computed_min_width),
                                   maybe_margin_left,
                                   maybe_margin_right,
                                   available_width)
            } else {
                (width, margin_left, margin_right)
            }
        };

        return (width, margin_left, margin_right);
    }

    pub fn compute_float_margins(&self, box_: &Box, remaining_width: Au) -> (Au, Au, Au) {
        let style = box_.style();
        let margin_left = MaybeAuto::from_style(style.Margin.margin_left,
                                                remaining_width).specified_or_zero();
        let margin_right = MaybeAuto::from_style(style.Margin.margin_right,
                                                 remaining_width).specified_or_zero();
        let shrink_to_fit = geometry::min(self.base.pref_width,
                                          geometry::max(self.base.min_width, remaining_width));
        let width = MaybeAuto::from_style(style.Box.width,
                                          remaining_width).specified_or_default(shrink_to_fit);
        debug!("assign_widths_float -- width: {}", width);
        return (width, margin_left, margin_right);
    }

    /// Calculates clearance, top_offset, bottom_offset, and left_offset for the box.
    /// If `ignore_clear` is true, clearance does not need to be calculated.
    pub fn initialize_offsets(&mut self, ignore_clear: bool) -> (Au, Au, Au, Au) {
        match self.box_ {
            None => (Au(0), Au(0), Au(0), Au(0)),
            Some(ref box_) => {
                let clearance = match box_.clear() {
                    Some(clear) if !ignore_clear => self.base.floats_in.clearance(clear),
                    _ => Au::new(0),
                };

                let top_offset = clearance + box_.noncontent_top();
                let bottom_offset = box_.noncontent_bottom();
                let left_offset = box_.offset();

                (clearance, top_offset, bottom_offset, left_offset)
            }
        }
    }

    /// Computes margin_top and margin_bottom. Also, it is decided whether top margin and
    /// bottom margin are collapsible according to CSS 2.1 § 8.3.1.
    pub fn precompute_margin(&mut self) -> (Au, Au, bool, bool) {
        match self.box_ {
            None => (Au(0), Au(0), false, false),
            Some(ref box_) => {
                let top_margin_collapsible = !self.is_root &&
                                             box_.border.get().top == Au(0) &&
                                             box_.padding.get().top == Au(0);

                let bottom_margin_collapsible = !self.is_root &&
                                                box_.border.get().bottom == Au(0) &&
                                                box_.padding.get().bottom == Au(0);

                let margin_top = box_.margin.get().top;
                let margin_bottom = box_.margin.get().bottom;

                (margin_top, margin_bottom, top_margin_collapsible, bottom_margin_collapsible)
            }
        }
    }

    /// Computes collapsed margins between adjacent children or between the first/last child and parent
    /// according to CSS 2.1 § 8.3.1. Current y position(cur_y) is continually updated for collapsing result.
    pub fn compute_margin_collapse(&mut self,
                                   cur_y: &mut Au,
                                   top_offset: &mut Au,
                                   margin_top: &mut Au,
                                   margin_bottom: &mut Au,
                                   top_margin_collapsible: bool,
                                   bottom_margin_collapsible: bool) {
        let mut first_in_flow = true;
        let mut collapsing = Au::new(0);
        let mut collapsible = if top_margin_collapsible {
            *margin_top
        } else {
            Au(0)
        };
        for kid in self.base.child_iter() {
            kid.collapse_margins(top_margin_collapsible,
                                 &mut first_in_flow,
                                 margin_top,
                                 top_offset,
                                 &mut collapsing,
                                 &mut collapsible);

            let child_node = flow::mut_base(*kid);
            *cur_y = *cur_y - collapsing;
            child_node.position.origin.y = *cur_y;
            *cur_y = *cur_y + child_node.position.size.height;
        }

        // The bottom margin collapses with its last in-flow block-level child's bottom margin
        // if the parent has no bottom boder, no bottom padding.
        collapsing = if bottom_margin_collapsible {
            if *margin_bottom < collapsible {
                *margin_bottom = collapsible;
            }
            collapsible
        } else {
            Au::new(0)
        };

        *cur_y = *cur_y - collapsing;
    }

    /// In case of inorder assign_height traversal, 'assign_height's of all children are visited.
    /// FloatContext info is shared between adjacent children.
    /// FloatContext info of the last child is returned.
    pub fn handle_children_floats_if_inorder(&mut self,
                                             ctx: &mut LayoutContext,
                                             point: Point2D<Au>,
                                             inorder: bool)
                                             -> FloatContext {
        if inorder {
            // Floats for blocks work like this:
            // self.floats_in -> child[0].floats_in
            // visit child[0]
            // child[i-1].floats_out -> child[i].floats_in
            // visit child[i]
            // repeat until all children are visited.
            // last_child.floats_out -> self.floats_out (done at the end of this method)
            let mut float_ctx = self.base.floats_in.translate(point);
            for kid in self.base.child_iter() {
                flow::mut_base(*kid).floats_in = float_ctx.clone();
                kid.assign_height_inorder(ctx);
                float_ctx = flow::mut_base(*kid).floats_out.clone();
            }
            float_ctx
        } else {
            Invalid
        }
    }

    /// Computes own height and sets position and margin of the box.
    pub fn compute_height_position(&mut self,
                                   height: &mut Au,
                                   screen_height: Au,
                                   border_and_padding: Au,
                                   margin_top: Au,
                                   margin_bottom: Au,
                                   clearance: Au,
                                   top_offset: Au) {
        let mut noncontent_height = Au::new(0);
        for box_ in self.box_.iter() {
            let mut position = box_.position.get();
            let mut margin = box_.margin.get();

            // The associated box is the border box of this flow.
            margin.top = margin_top;
            margin.bottom = margin_bottom;

            let (y, h) = box_.get_y_coord_and_new_height_if_fixed(screen_height,
                                                                  *height,
                                                                  clearance + margin.top,
                                                                  self.is_fixed);

            position.origin.y = y;
            *height = h;

            if self.is_fixed {
                for kid in self.base.child_iter() {
                    let child_node = flow::mut_base(*kid);
                    child_node.position.origin.y = position.origin.y + top_offset;
                }
            }

            position.size.height = if self.is_fixed {
                *height
            } else {
                *height + border_and_padding
            };

            noncontent_height = border_and_padding + clearance + margin.top + margin.bottom;

            box_.position.set(position);
            box_.margin.set(margin);
        }

        self.base.position.size.height = if self.is_fixed {
            *height
        } else {
            *height + noncontent_height
        };
    }

    /// Sets floats_out at the last step of the assign height calculation.
    pub fn set_floats_out(&mut self,
                          float_ctx: &mut FloatContext,
                          height: Au,
                          cur_y: Au,
                          top_offset: Au,
                          bottom_offset: Au,
                          left_offset: Au,
                          inorder: bool) {
        if inorder {
            let extra_height = height - (cur_y - top_offset) + bottom_offset;
            self.base.floats_out = float_ctx.translate(Point2D(left_offset, -extra_height));
        } else {
            self.base.floats_out = self.base.floats_in.clone();
        }
    }

    // inline(always) because this is only ever called by in-order or non-in-order top-level
    // methods
    #[inline(always)]
    fn assign_height_block_base(&mut self, ctx: &mut LayoutContext, inorder: bool) {

        let (clearance, top_offset, bottom_offset, left_offset) = self.initialize_offsets(false);

        let mut float_ctx = self.handle_children_floats_if_inorder(ctx,
                                                                   Point2D(-left_offset, -top_offset),
                                                                   inorder);

        let (mut margin_top, mut margin_bottom, top_margin_collapsible, bottom_margin_collapsible) = self.precompute_margin();

        let mut cur_y = top_offset;
        let mut top_offset = top_offset;
        self.compute_margin_collapse(&mut cur_y,
                                     &mut top_offset,
                                     &mut margin_top,
                                     &mut margin_bottom,
                                     top_margin_collapsible,
                                     bottom_margin_collapsible);

        // TODO: A box's own margins collapse if the 'min-height' property is zero, and it has neither
        // top or bottom borders nor top or bottom padding, and it has a 'height' of either 0 or 'auto',
        // and it does not contain a line box, and all of its in-flow children's margins (if any) collapse.

        let screen_height = ctx.screen_size.height;

        let mut height = if self.is_root {
            // FIXME(pcwalton): The max is taken here so that you can scroll the page, but this is
            // not correct behavior according to CSS 2.1 § 10.5. Instead I think we should treat
            // the root element as having `overflow: scroll` and use the layers-based scrolling
            // infrastructure to make it scrollable.
            Au::max(screen_height, cur_y)
        } else {
            cur_y - top_offset
        };

        let mut border_and_padding = Au::new(0);
        for box_ in self.box_.iter() {
            let style = box_.style();

            // At this point, `height` is the height of the containing block, so passing `height`
            // as the second argument here effectively makes percentages relative to the containing
            // block per CSS 2.1 § 10.5.
            height = match MaybeAuto::from_style(style.Box.height, height) {
                Auto => height,
                Specified(value) => value
            };

            border_and_padding = box_.padding.get().top + box_.padding.get().bottom +
                box_.border.get().top + box_.border.get().bottom;
        }

        self.compute_height_position(&mut height,
                                     screen_height,
                                     border_and_padding,
                                     margin_top,
                                     margin_bottom,
                                     clearance,
                                     top_offset);

        self.set_floats_out(&mut float_ctx, height, cur_y, top_offset,
                            bottom_offset, left_offset, inorder);
    }

    pub fn assign_height_float_inorder(&mut self) {
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

    pub fn assign_height_float(&mut self, ctx: &mut LayoutContext) {
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
            top_offset = box_.noncontent_top();
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

    /// In case of float, initializes containing_width at the beginning step of assign_width.
    pub fn set_containing_width_if_float(&mut self, remaining_width: Au) {
        if self.is_float() {
            self.float.get_mut_ref().containing_width = remaining_width;

            // Parent usually sets this, but floats are never inorder
            self.base.flags_info.flags.set_inorder(false);
        }
    }

    /// Caculates padding or margin of the box before computing own width.
    /// If margin exists, it returns the computed 'width' value according to CSS 2.1 § 10.3.
    pub fn compute_padding_and_margin_if_exists(&self, box_: &Box, style: &ComputedValues,
                                                remaining_width: Au,
                                                has_padding: bool,
                                                has_margin: bool) -> Au {
        box_.assign_width(remaining_width);
        if has_padding {
            // Can compute padding here since we know containing block width.
            box_.compute_padding(style, remaining_width);
        }

        if has_margin {
            // Margins are 0 right now so base.noncontent_width() is just borders + padding.
            let available_width = remaining_width - box_.noncontent_width();

            // Top and bottom margins for blocks are 0 if auto.
            let margin_top = MaybeAuto::from_style(style.Margin.margin_top,
                                                   remaining_width).specified_or_zero();
            let margin_bottom = MaybeAuto::from_style(style.Margin.margin_bottom,
                                                      remaining_width).specified_or_zero();

            let (width, margin_left, margin_right) = if self.is_float() {
                self.compute_float_margins(box_, remaining_width)
            } else {
                self.compute_block_margins(box_, remaining_width, available_width)
            };

            box_.margin.set(SideOffsets2D::new(margin_top,
                                               margin_right,
                                               margin_bottom,
                                               margin_left));
            return width;
        }
        Au(0)
    }

    /// The position of the box is set with the given x and width.
    pub fn set_box_x_and_width(&self, box_: &Box, border_x: Au, border_width: Au) {
        let mut position_ref = box_.position.borrow_mut();
        position_ref.get().origin.x = border_x;
        position_ref.get().size.width = border_width;
    }

    /// Assigns the computed x_offset and width to children.
    pub fn propagate_assigned_width_to_children(&mut self, x_offset: Au,
                                                remaining_width: Au,
                                                opt_col_widths: Option<~[Au]>) {
        let has_inorder_children = if self.is_float() {
            self.base.num_floats > 0
        } else {
            self.base.flags_info.flags.inorder() || self.base.num_floats > 0
        };

        // FIXME(ksh8281): avoid copy
        let flags_info = self.base.flags_info.clone();
        let mut kid_x_offset = x_offset;
        let mut kid_width = remaining_width;
        for (i, kid) in self.base.child_iter().enumerate() {
            assert!(kid.starts_block_flow() || kid.starts_inline_flow() || kid.is_table_kind());
            match opt_col_widths {
                Some(ref col_widths) => {
                    // If kid is table_rowgroup or table_row, the column widths info should be
                    // copied from its parent.
                    if kid.is_table_rowgroup() {
                        kid.as_table_rowgroup().col_widths = col_widths.clone()
                    } else if kid.is_table_row() {
                        kid.as_table_row().col_widths = col_widths.clone()
                    } else if kid.is_table_cell() {
                        // If kid is table_cell, the x offset and width for each cell should be
                        // calculated from parent's column widths info.
                        kid_x_offset = if i == 0 {
                            Au(0)
                        } else {
                            kid_x_offset + col_widths[i-1]
                        };
                        kid_width = col_widths[i]
                    }
                }
                None => {}
            }

            let child_base = flow::mut_base(*kid);
            child_base.position.origin.x = kid_x_offset;
            child_base.position.size.width = kid_width;
            child_base.flags_info.flags.set_inorder(has_inorder_children);

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

    pub fn build_display_list_block<E:ExtraDisplayListData>(
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

        debug!("build_display_list_block: adding display element");

        // add box that starts block context
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
        // add box that starts block context
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

impl Flow for BlockFlow {
    fn class(&self) -> FlowClass {
        BlockFlowClass
    }

    fn as_block<'a>(&'a mut self) -> &'a mut BlockFlow {
        self
    }

    /* Recursively (bottom-up) determine the context's preferred and
    minimum widths.  When called on this context, all child contexts
    have had their min/pref widths set. This function must decide
    min/pref widths based on child context widths and dimensions of
    any boxes it is responsible for flowing.  */

    /* TODO: absolute contexts */
    /* TODO: inline-blocks */
    fn bubble_widths(&mut self, _: &mut LayoutContext) {
        let mut min_width = Au::new(0);
        let mut pref_width = Au::new(0);
        let mut num_floats = 0;

        /* find max width from child block contexts */
        for child_ctx in self.base.child_iter() {
            assert!(child_ctx.starts_block_flow() || child_ctx.starts_inline_flow() || child_ctx.is_table_kind());

            let child_base = flow::mut_base(*child_ctx);
            min_width = geometry::max(min_width, child_base.min_width);
            pref_width = geometry::max(pref_width, child_base.pref_width);
            num_floats = num_floats + child_base.num_floats;
        }

        if self.is_float() {
            self.base.num_floats = 1;
            self.float.get_mut_ref().floated_children = num_floats;
        } else {
            self.base.num_floats = num_floats;
        }

        /* if not an anonymous block context, add in block box's widths.
           these widths will not include child elements, just padding etc. */
        for box_ in self.box_.iter() {
            {
                // Can compute border width here since it doesn't depend on anything.
                box_.compute_borders_if_necessary(box_.style())
            }

            let (this_minimum_width, this_preferred_width) = box_.minimum_and_preferred_widths();
            min_width = min_width + this_minimum_width;
            pref_width = pref_width + this_preferred_width;
        }

        self.base.min_width = min_width;
        self.base.pref_width = pref_width;
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
                   "block"
               },
               self.base.id);

        if self.is_root {
            debug!("Setting root position");
            self.base.position.origin = Au::zero_point();
            self.base.position.size.width = ctx.screen_size.width;
            self.base.floats_in = FloatContext::new(self.base.num_floats);
            self.base.flags_info.flags.set_inorder(false);
        }

        // The position was set to the containing block by the flow's parent.
        let mut remaining_width = self.base.position.size.width;
        let mut x_offset = Au::new(0);

        self.set_containing_width_if_float(remaining_width);

        for box_ in self.box_.iter() {
            let style = box_.style();

            // The text alignment of a block flow is the text alignment of its box's style.
            self.base.flags_info.flags.set_text_align(style.Text.text_align);

            let width = self.compute_padding_and_margin_if_exists(box_, style, remaining_width, true, true);
            let screen_size = ctx.screen_size;
            let (x, w) = box_.get_x_coord_and_new_width_if_fixed(screen_size.width,
                                                                 screen_size.height,
                                                                 width,
                                                                 box_.offset(),
                                                                 self.is_fixed);

            x_offset = x;
            remaining_width = w;

            let border_x = if self.is_fixed {
                let border_x = x_offset + box_.margin.get().left;
                x_offset = x_offset + box_.padding.get().left;
                border_x
            } else {
                box_.margin.get().left
            };
            let padding_and_borders = box_.padding.get().left + box_.padding.get().right +
                                      box_.border.get().left + box_.border.get().right;
            // The associated box is the border box of this flow.
            self.set_box_x_and_width(box_, border_x, remaining_width + padding_and_borders);
        }

        if self.is_float() {
            self.base.position.size.width = remaining_width;
        }

        self.propagate_assigned_width_to_children(x_offset, remaining_width, None);
    }

    fn assign_height_inorder(&mut self, ctx: &mut LayoutContext) {
        if self.is_float() {
            debug!("assign_height_inorder_float: assigning height for float {}", self.base.id);
            self.assign_height_float_inorder();
        } else {
            debug!("assign_height_inorder: assigning height for block {}", self.base.id);
            self.assign_height_block_base(ctx, true);
        }
    }

    fn assign_height(&mut self, ctx: &mut LayoutContext) {
        //assign height for box
        for box_ in self.box_.iter() {
            box_.assign_height();
        }

        if self.is_float() {
            debug!("assign_height_float: assigning height for float {}", self.base.id);
            self.assign_height_float(ctx);
        } else {
            debug!("assign_height: assigning height for block {}", self.base.id);
            // This is the only case in which a block flow can start an inorder
            // subtraversal.
            if self.is_root && self.base.num_floats > 0 {
                self.assign_height_inorder(ctx);
                return;
            }
            self.assign_height_block_base(ctx, false);
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

    fn mark_as_root(&mut self) {
        self.is_root = true
    }

    fn debug_str(&self) -> ~str {
        let txt = if self.is_float() {
            ~"FloatFlow: "
        } else if self.is_root {
            ~"RootFlow: "
        } else {
            ~"BlockFlow: "
        };
        txt.append(match self.box_ {
            Some(ref rb) => rb.debug_str(),
            None => ~"",
        })
    }
}

